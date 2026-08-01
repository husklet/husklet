//! Scissor and viewport — the two pass rectangles the oracle used to ignore outright.
//!
//! WHY these exist: the oracle applied NEITHER, so a wgpu-vs-oracle differential over any scissored or
//! viewport-transformed program agreed by both sides being wrong. Every value below is hand-derived from
//! the rectangle, not captured from an executor.
//!
//! The last test pins the IR shape of a GL scissored `glClear`: WebGPU's `LoadOp::Clear` is a load-op with
//! no scissor concept, so a scissor-tested clear MUST lower to `Enc::ClearRect` over a `LoadOp::Load` pass.
//! A lowering that folds it into the load-op paints the whole attachment — the `egl_offscreen` failure.

use super::*;

/// A fullscreen CCW triangle (covers the whole target) in opaque green, stride 24.
fn fullscreen_green() -> Vec<u8> {
    [
        ((-1.0f32, -1.0f32), [0.0, 1.0, 0.0, 1.0]),
        ((3.0, -1.0), [0.0, 1.0, 0.0, 1.0]),
        ((-1.0, 3.0), [0.0, 1.0, 0.0, 1.0]),
    ]
    .iter()
    .flat_map(|((x, y), c)| vtx24(*x, *y, *c))
    .collect()
}

/// Program: clear an 8x8 target to opaque red, then draw `fullscreen_green` through `rect_ops`
/// (the `SetScissor`/`SetViewport` ops under test). Returns the 8x8 RGBA readback.
fn draw_through(rect_ops: Vec<Enc>) -> Vec<u8> {
    let verts = fullscreen_green();
    let mut encoder = vec![
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: 1,
                load: LoadOp::Clear,
                clear: [1.0, 0.0, 0.0, 1.0],
                store: true,
            }],
            depth: None,
        },
        Enc::SetPipeline(1),
        Enc::SetVertexBuffer {
            slot: 0,
            buffer: 1,
            offset: 0,
        },
    ];
    encoder.extend(rect_ops);
    encoder.push(Enc::Draw {
        vertex_count: 3,
        instance_count: 1,
        first_vertex: 0,
        first_instance: 0,
    });
    encoder.push(Enc::EndRenderPass);

    let (exec, s) = run(&[
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kernel_words(),
        },
        draw_pipeline(1, None, 0xF, 0, 0, 24, 8),
        Cmd::CreateTexture(
            1,
            tex(
                8,
                8,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
        Cmd::CreateBuffer(
            1,
            buf(
                verts.len() as u64,
                buffer_usage::VERTEX | buffer_usage::COPY_DST,
            ),
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: verts,
        },
        Cmd::Submit(CommandBuffer {
            encoder,
            signal: None,
        }),
    ]);
    readback(&exec, &s, 1, 8 * 8 * 4)
}

/// The set of pixels (x, y) whose texel equals `want`.
fn extent(px: &[u8], want: [u8; 4]) -> Vec<(usize, usize)> {
    (0..8)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .filter(|(x, y)| px[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4] == want)
        .collect()
}

const GREEN: [u8; 4] = [0, 255, 0, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

#[test]
fn scissor_clips_a_draw_to_its_rectangle() {
    let px = draw_through(vec![Enc::SetScissor {
        x: 2,
        y: 1,
        w: 3,
        h: 4,
    }]);
    let green = extent(&px, GREEN);
    // Exactly the 3x4 rect at (2,1) — the rest stays the red clear.
    let want: Vec<(usize, usize)> = (1..5).flat_map(|y| (2..5).map(move |x| (x, y))).collect();
    assert_eq!(green, want, "scissored draw must fill only its rectangle");
    assert_eq!(extent(&px, RED).len(), 64 - 12, "outside stays cleared");
}

#[test]
fn scissor_outside_the_target_draws_nothing() {
    let px = draw_through(vec![Enc::SetScissor {
        x: 8,
        y: 8,
        w: 4,
        h: 4,
    }]);
    assert_eq!(
        extent(&px, RED).len(),
        64,
        "an empty scissor intersection rasterizes nothing"
    );
}

#[test]
fn viewport_maps_ndc_into_its_subrectangle() {
    // A fullscreen NDC triangle through a 4x2 viewport at (4,6) covers exactly that rect and nothing else.
    let px = draw_through(vec![Enc::SetViewport {
        x: 4.0,
        y: 6.0,
        w: 4.0,
        h: 2.0,
        min_depth: 0.0,
        max_depth: 1.0,
    }]);
    let want: Vec<(usize, usize)> = (6..8).flat_map(|y| (4..8).map(move |x| (x, y))).collect();
    assert_eq!(extent(&px, GREEN), want, "viewport must transform AND clip");
}

#[test]
fn scissor_intersects_the_viewport() {
    // Viewport (0,0,8,4) ∩ scissor (2,2,8,8) = (2,2)..(8,4).
    let px = draw_through(vec![
        Enc::SetViewport {
            x: 0.0,
            y: 0.0,
            w: 8.0,
            h: 4.0,
            min_depth: 0.0,
            max_depth: 1.0,
        },
        Enc::SetScissor {
            x: 2,
            y: 2,
            w: 8,
            h: 8,
        },
    ]);
    let want: Vec<(usize, usize)> = (2..4).flat_map(|y| (2..8).map(move |x| (x, y))).collect();
    assert_eq!(extent(&px, GREEN), want, "both rectangles must apply");
}

#[test]
fn a_scissored_clear_lowers_to_clear_rect_over_a_load_pass() {
    // The IR shape a scissor-tested `glClear` MUST take: the pass PRESERVES the attachment (`LoadOp::Load`)
    // and the clear is an `Enc::ClearRect` over the scissor rectangle. Folding it into the pass load-op
    // instead repaints all 64 texels — the measured `egl_offscreen` failure.
    let (exec, s) = run(&[
        Cmd::CreateTexture(
            1,
            tex(
                8,
                8,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST,
            ),
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                // Frame 1: a full clear to red.
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [1.0, 0.0, 0.0, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
                // Frame 2: `glEnable(GL_SCISSOR_TEST); glScissor(0,0,4,4); glClear(green)`.
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Load,
                        clear: [0.0, 0.0, 0.0, 0.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
                Enc::ClearRect {
                    texture: 1,
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 4,
                    color: [0.0, 1.0, 0.0, 1.0],
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 0,
                },
            ],
            signal: None,
        }),
    ]);
    let px = readback(&exec, &s, 1, 8 * 8 * 4);
    let want: Vec<(usize, usize)> = (0..4).flat_map(|y| (0..4).map(move |x| (x, y))).collect();
    assert_eq!(
        extent(&px, GREEN),
        want,
        "scissored clear fills only its rect"
    );
    assert_eq!(
        extent(&px, RED).len(),
        64 - 16,
        "the rest survives the clear"
    );
}

/// Which kind of refusal a subresource case must produce — see the test below for why the two differ.
#[derive(Clone, Copy, Debug)]
enum Refusal {
    /// The subresource does not exist on this texture.
    OutOfBounds,
    /// The subresource is one no texture in this reference has. Currently unused — see the test's note.
    #[allow(dead_code)]
    Unsupported,
}

/// A clear must never be redirected to a subresource the caller did not name. Silently writing layer 0 in
/// response to a clear of some other subresource is precisely the defect the subresource fields were added
/// to fix, and a backend that reproduced it here would make the wgpu-vs-oracle differential agree by both
/// sides being wrong.
///
/// The premise changed under this test and the cases did not. It used to hold because the oracle
/// materialized ONE plane per texture and refused a layered texture at creation, so every non-base
/// subresource was categorically unsupported. It now materializes one plane per LAYER and serves a layer
/// range, which the executor does too. What that changes here is the REASON each case is refused, not
/// whether it is:
///
///   * the two LAYER cases name layers this four-by-four single-layer texture does not have, so they are
///     out of bounds — the same answer a range past the end gets on a texture that is layered (see
///     `oracle_spec::layered`). They are no longer categorically unsupported, and asserting that they were
///     would now be asserting the old limit rather than the rule.
///   * the MIP case has since followed them, for the same reason. The reference materializes the whole
///     mip pyramid now, so level 1 is not a level no texture has — it is a level THIS single-level
///     texture does not have, which is a bound and not a missing capability.
///
/// All three are now out of bounds, and the enum below has one inhabited variant. It is kept rather than
/// collapsed because the distinction it draws is real and load-bearing: a subresource this texture lacks
/// and a subresource no texture here can have are different answers, and the second one WILL return the
/// moment a shape or level is added that the reference genuinely cannot serve.
///
/// The refusal is raised during validation, before any op in the batch runs, so an earlier op's writes are
/// never left behind by a mid-batch rejection.
#[test]
fn oracle_refuses_a_clear_of_a_non_base_subresource() {
    let mut exec = hl_gpu::CpuExecutor::new();
    let caps = exec.capabilities();
    let mut limits = hl_gpu::Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = hl_gpu::Session::new(
        limits,
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    // A plain single-layer, single-mip 2D texture. The refusal under test must come from the SUBRESOURCE
    // the clear names against THIS texture, not from the texture's shape being unsupported — which is
    // what keeps this a test about the clear rather than a restatement of a create-time refusal.
    let desc = tex(
        4,
        4,
        TextureFormat::Rgba8Unorm,
        texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST,
    );
    for (label, expected, clear) in [
        (
            "a non-base array layer",
            Refusal::OutOfBounds,
            Enc::ClearRect {
                texture: 1,
                x: 0,
                y: 0,
                w: 4,
                h: 4,
                color: [1.0, 0.0, 0.0, 1.0],
                base_array_layer: 2,
                layer_count: 1,
                mip_level: 0,
            },
        ),
        (
            "more than one array layer",
            Refusal::OutOfBounds,
            Enc::ClearRect {
                texture: 1,
                x: 0,
                y: 0,
                w: 4,
                h: 4,
                color: [1.0, 0.0, 0.0, 1.0],
                base_array_layer: 0,
                layer_count: 2,
                mip_level: 0,
            },
        ),
        (
            "a non-base mip level",
            Refusal::OutOfBounds,
            Enc::ClearRect {
                texture: 1,
                x: 0,
                y: 0,
                w: 2,
                h: 2,
                color: [1.0, 0.0, 0.0, 1.0],
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 1,
            },
        ),
    ] {
        let err = hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[
                Cmd::CreateTexture(1, desc.clone()),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![clear],
                    signal: None,
                }),
                Cmd::DestroyTexture(1),
            ],
        )
        .expect_err(label);
        let ok = match expected {
            Refusal::OutOfBounds => matches!(err, hl_gpu::GpuError::OutOfBounds),
            Refusal::Unsupported => matches!(err, hl_gpu::GpuError::Unsupported(_)),
            };
        assert!(ok, "{label} must be refused as {expected:?}, got {err:?}");
    }

    // And the base subresource still runs, so the refusal is about the subresource and not about
    // `ClearRect` generally.
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(2, desc),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ClearRect {
                    texture: 2,
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 4,
                    color: [1.0, 0.0, 0.0, 1.0],
                    base_array_layer: 0,
                    layer_count: 1,
                    mip_level: 0,
                }],
                signal: None,
            }),
        ],
    )
    .expect("the base subresource is still clearable");
}
