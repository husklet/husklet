use super::*;

/// The full pipeline on a good batch: negotiate succeeds, then validate → account → dispatch runs the
/// executor, charges residency, and stamps the fence timeline.
#[test]
fn negotiate_then_submit_good_batch_executes_and_accounts() {
    let caps = Capabilities::permissive_fixture("fake");
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
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 0,
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
    let caps = Capabilities::permissive_fixture("fake");
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

/// Capability honesty in the tightening direction: the CPU reference executor MATERIALIZES a combined
/// depth+stencil attachment (an 8-byte/texel depth+stencil plane) and runs the full stencil test/op set
/// against it, so it must ADVERTISE `Depth24PlusStencil8`. While it did not, a guest could negotiate the
/// stencil encoder ops successfully and then have every stencil attachment rejected at validation as an
/// unsupported format — a capability the intersection claimed but the format bitset withheld.
#[test]
fn the_cpu_oracle_advertises_the_depth_stencil_format_it_materializes() {
    use hl_gpu::protocol::model::descriptor::TextureDesc;
    use hl_gpu::{CommandSink, CpuExecutor, InProcessCommandSink};

    let exec = CpuExecutor::new();
    assert!(
        exec.capabilities()
            .supports_format(TextureFormat::Depth24PlusStencil8),
        "the oracle implements this format, so it must advertise it"
    );

    // And a guest can actually create one through the whole runtime pipeline, with no widened ceilings.
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    sink.submit(&[Cmd::CreateTexture(
        1,
        TextureDesc {
            width: 4,
            height: 4,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Depth24PlusStencil8,
            usage: texture_usage::RENDER_TARGET,
            label: String::new(),
        },
    )])
    .expect("a depth+stencil render target the oracle materializes must validate");
}

/// The other direction of capability honesty: the oracle must not ADVERTISE an encoder op it refuses.
/// It materializes only mip 0 of a single layer, so the two explicit-region buffer↔texture copies are
/// rejected outright at replay — a guest requiring them has to learn that at NEGOTIATION, cleanly, not
/// after the app already committed to the path and a frame comes back `Unsupported`.
#[test]
fn the_cpu_oracle_does_not_advertise_the_region_copies_it_refuses() {
    use hl_gpu::protocol::model::command::etag;
    use hl_gpu::CpuExecutor;

    let caps = CpuExecutor::new().capabilities();
    assert!(!caps.supports_command(etag::COPY_B2T_REGION));
    assert!(!caps.supports_command(etag::COPY_T2B_REGION));
    // Everything else in the advertised set stays advertised.
    assert!(caps.supports_command(etag::COPY_T2T) && caps.supports_command(etag::BLIT_TEXTURE));

    assert_eq!(
        caps.negotiate(&FeatureRequest {
            wire_version: caps.wire_version,
            command_bits: hl_gpu::Capabilities::command_bits(&[etag::COPY_B2T_REGION]),
            ..FeatureRequest::default()
        })
        .unwrap_err(),
        GpuError::Unsupported("capability: command tag not supported"),
    );
}
