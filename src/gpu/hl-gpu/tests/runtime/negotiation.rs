use super::*;

/// The full pipeline on a good batch: negotiate succeeds, then validate → account → dispatch runs the
/// executor, charges residency, and stamps the fence timeline.
#[test]
fn negotiate_then_submit_good_batch_executes_and_accounts() {
    let caps = Capabilities::full("fake");
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(
        Limits::from_capabilities(caps.clone()),
        GlobalLedger::unbounded(),
    );

    // Negotiate the full current IR surface against the executor's advertised caps.
    let req = FeatureRequest {
        wire_version: caps.wire_version,
        shader_payloads: shader_payload::SPIRV,
        command_bits: hl_gpu::Capabilities::command_bits(ALL_COMMANDS),
        texture_formats: 0,
        ..FeatureRequest::default()
    };
    let negotiated = service::negotiate::negotiate(&mut s, &exec, &req).expect("negotiate ok");
    assert_eq!(negotiated.name, "fake");
    assert!(s.caps.is_some());

    // A representative batch: create a buffer + surface + fence, submit a clear signalling the fence,
    // then present.
    let batch = vec![
        buffer(1, 4096),
        Cmd::CreateSurface(
            10,
            SurfaceDesc {
                width: 4,
                height: 4,
                format: TextureFormat::Rgba8Unorm,
                token: hl_gpu::SurfaceToken::new(1).unwrap(),
            },
        ),
        Cmd::CreateFence(20),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::ClearRect {
                texture: 10,
                x: 0,
                y: 0,
                w: 4,
                h: 4,
                color: [0.0; 4],
            }],
            signal: Some((20, 7)),
        }),
        Cmd::Present {
            surface: 10,
            texture: 10,
            serial: hl_gpu::FrameSerial::new(99).unwrap(),
        },
    ];
    let presents = hl_gpu::runtime::submit(&mut s, &mut exec, 512, &batch).expect("good batch");

    assert_eq!(
        presents,
        vec![Presentation {
            surface: SurfaceId(10),
            token: hl_gpu::SurfaceToken::new(1).unwrap(),
            texture: TextureId(10),
            serial: hl_gpu::FrameSerial::new(99).unwrap(),
        }]
    );
    assert_eq!(
        exec.command_count(),
        batch.len(),
        "the whole batch reached the executor"
    );
    // Residency: 4096 (buffer) + 64 (4x4 rgba8 surface) + 128 (fence) across 3 objects.
    assert_eq!(s.residency_bytes(), 4096 + 4 * 4 * 4 + 128);
    assert_eq!(s.object_count(), 3);
    assert_eq!(
        s.resources.live_count(),
        3,
        "executor tracked natives behind the ids"
    );
    // Fence 20 signalled to 7, stamped with the fake clock.
    assert_eq!(s.timeline.get(20), Some(7));
    assert!(s.timeline.is_reached(20, 7) && !s.timeline.is_reached(20, 8));
}

/// Failure atomicity: an over-`max_buffer_bytes` create is rejected at VALIDATE — before the executor is
/// ever called and before any residency is charged.
/// Negotiation rejects an incompatible guest before any command flows.
#[test]
fn negotiate_rejects_incompatible_wire_version() {
    let caps = Capabilities::full("fake");
    let exec = FakeExecutor::new(caps.clone());
    let mut s = session(
        Limits::from_capabilities(caps.clone()),
        GlobalLedger::unbounded(),
    );
    let req = FeatureRequest {
        wire_version: caps.wire_version + 1,
        ..Default::default()
    };
    assert_eq!(
        service::negotiate::negotiate(&mut s, &exec, &req).unwrap_err(),
        GpuError::Unsupported("capability: wire version mismatch")
    );
    assert!(s.caps.is_none(), "a failed negotiation records no caps");
}
