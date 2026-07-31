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
