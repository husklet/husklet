use super::*;

// ---------------------------------------------------------------------------------------------------
// glClearBufferfv → a scoped clear pass
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_buffer_color_lowers_to_a_clear_pass() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    record::clear_buffer_color(&mut c, 0, [0.25, 0.5, 0.75, 1.0]);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);
    match &ops[0] {
        Enc::BeginRenderPass { color, .. } => {
            assert_eq!(color[0].clear, [0.25, 0.5, 0.75, 1.0]);
        }
        other => panic!("expected a clear pass, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------
// The depth/stencil clear VALUE belongs to the recorded clear, and glDepthMask gates it
// ---------------------------------------------------------------------------------------------------

fn pass_depth_clear(batch: &[Cmd]) -> Option<(f32, u32)> {
    submit_ops(batch).iter().find_map(|e| match e {
        Enc::BeginRenderPass { depth, .. } => {
            depth.as_ref().map(|d| (d.clear_depth, d.clear_stencil))
        }
        _ => None,
    })
}

#[test]
fn depth_clear_uses_the_value_in_force_at_the_clear_not_at_lowering() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::depth_mask(&mut c, true);
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    // glClearDepthf is ordinary state; moving it AFTER the clear must not retroactively change the clear.
    record::clear_depth(&mut c, 1.0);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let (clear_depth, _) = pass_depth_clear(&sink.batches[0]).expect("a depth attachment");
    assert_eq!(
        clear_depth, 0.5,
        "the clear recorded at 0.5 must clear to 0.5"
    );
}

#[test]
fn a_masked_off_depth_clear_leaves_the_plane_at_the_earlier_clears_value() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::depth_mask(&mut c, true);
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    // glDepthMask(GL_FALSE) makes a depth clear a no-op (ES 3.0 §4.2.3), so the 1.0 never lands.
    record::depth_mask(&mut c, false);
    record::clear_depth(&mut c, 1.0);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    record::depth_mask(&mut c, true);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let (clear_depth, _) = pass_depth_clear(&sink.batches[0]).expect("a depth attachment");
    assert_eq!(
        clear_depth, 0.5,
        "the masked-off clear must not overwrite the 0.5 the open-masked clear left"
    );
}

// ---------------------------------------------------------------------------------------------------
// An FBO with no depth attachment: every depth test passes and nothing is written (ES 3.0 §4.1.5)
// ---------------------------------------------------------------------------------------------------

/// Bind a fresh FBO with one sized RGBA colour texture and, when asked, a depth renderbuffer.
fn bind_offscreen(c: &mut GlContext, with_depth: bool) {
    let tex = c.textures.gen();
    record::bind_texture(c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(c, 64, 64, &[]);
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );
    if with_depth {
        let rbo = c.gen_renderbuffer();
        record::bind_renderbuffer(c, GL_RENDERBUFFER, rbo);
        record::renderbuffer_storage(c, GL_RENDERBUFFER, GL_DEPTH_COMPONENT16, 64, 64);
        record::framebuffer_renderbuffer(
            c,
            GL_FRAMEBUFFER,
            GL_DEPTH_ATTACHMENT,
            GL_RENDERBUFFER,
            rbo,
        );
    }
}

fn depth_state_present(with_depth: bool) -> bool {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    bind_offscreen(&mut c, with_depth);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_func(&mut c, GL_LESS);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    pipeline_desc(&sink.batches[0]).depth.is_some()
}

#[test]
fn depth_test_on_a_depthless_fbo_carries_no_depth_state() {
    assert!(
        !depth_state_present(false),
        "with no GL_DEPTH_ATTACHMENT the depth test must always pass, so no depth state is lowered"
    );
}

#[test]
fn depth_test_on_an_fbo_with_a_depth_attachment_still_tests() {
    assert!(
        depth_state_present(true),
        "an attached depth renderbuffer must keep the depth test armed"
    );
}

// ---------------------------------------------------------------------------------------------------
// A scissored clear whose box hangs off the target must be CLIPPED, not slid inside it
// ---------------------------------------------------------------------------------------------------

fn clear_rect(batch: &[Cmd]) -> (u32, u32, u32, u32) {
    submit_ops(batch)
        .iter()
        .find_map(|e| match e {
            Enc::ClearRect { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            _ => None,
        })
        .expect("a ClearRect")
}

/// `glScissor(-10, -10, 24, 24)` on a 256×256 target leaves x ∈ [0, 14) and y ∈ [0, 14) inside — 24 minus
/// the 10 columns and rows clipped away. Clamping only the ORIGIN and keeping the full width re-added
/// those 10 columns on the far side, so the clear painted 24 columns from 0 and wrote x = 14 onward.
#[test]
fn a_scissored_clear_clipped_by_the_origin_loses_the_clipped_extent() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [-10, -10, 24, 24]);
    record::clear_color(&mut c, [0.0, 1.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    // Rows convert from GL's bottom-left origin. The box spans GL y ∈ [-10, 14); the part inside the
    // target is y ∈ [0, 14), i.e. 14 rows, and GL rows 0..13 are texture rows 242..255. So the rect is
    // top = 256 - (-10 + 24) = 242 with height 14 — clipped by the same 10 rows, at the far edge in
    // texture space. The OLD code derived the top from the box's far edge and so got the height right by
    // accident while getting the width wrong, which is exactly the single-axis defect that was observed.
    let (x, y, w, h) = clear_rect(&sink.batches[0]);
    assert_eq!(
        (x, w),
        (0, 14),
        "the 10 columns left of the target are gone"
    );
    assert_eq!((y, h), (242, 14), "and the 10 rows below it");
}

/// The same clamp at the FAR edge, which already worked and must keep working.
#[test]
fn a_scissored_clear_past_the_far_edge_is_truncated() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [250, 0, 40, 8]);
    record::clear_color(&mut c, [0.0, 1.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let (x, _, w, _) = clear_rect(&sink.batches[0]);
    assert_eq!((x, w), (250, 6));
}

/// A box entirely outside the target paints nothing at all.
#[test]
fn a_scissored_clear_wholly_outside_the_target_is_dropped() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [-40, 0, 20, 8]);
    record::clear_color(&mut c, [0.0, 1.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    assert!(
        !submit_ops(&sink.batches[0])
            .iter()
            .any(|e| matches!(e, Enc::ClearRect { .. })),
        "nothing of the box is inside the target"
    );
}

// ---------------------------------------------------------------------------------------------------
// A depth clear must land even when NO draw in that frame is depth-tested
// ---------------------------------------------------------------------------------------------------

/// The `state:depth_toggle` sequence: clear depth to 0.5, draw with the depth test OFF, present, then
/// enable `GL_DEPTH_TEST`/`GL_LESS` and draw again. The second draw sits at window depth 0.5, so
/// `0.5 < 0.5` is false and it must be rejected.
///
/// The pass's depth attachment was decided from the DRAWS alone. Frame 1's only draw was untested, so no
/// depth plane was materialized and the `glClearDepthf(0.5)` was silently dropped; frame 2 then minted the
/// plane fresh, clear-loaded it to the GL initial 1.0, and every fragment passed. The symptom — "enabling
/// the depth test between draws has no effect" — is one frame removed from its cause.
#[test]
fn a_depth_clear_materializes_the_plane_even_with_no_depth_tested_draw() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);

    // Frame 1: colour clear, depth clear to 0.5, one UNTESTED draw.
    record::clear_color(&mut c, [0.0, 0.25, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());

    let first = pass_depth_clear(&sink.batches[0]).expect(
        "the depth clear must materialize a depth plane even though no draw in this frame tests depth",
    );
    assert_eq!(first.0, 0.5, "cleared to the value the app asked for");

    // Frame 2: enable the depth test and draw again. The plane already exists, so it load-preserves the
    // 0.5 — which is what rejects the draw.
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_func(&mut c, GL_LESS);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());

    let ops = submit_ops(&sink.batches[1]);
    let load = ops
        .iter()
        .find_map(|e| match e {
            Enc::BeginRenderPass { depth, .. } => depth.as_ref().map(|d| d.load),
            _ => None,
        })
        .expect("frame 2 is depth-tested and must carry the depth attachment");
    assert_eq!(
        load,
        hl_gpu::protocol::model::enums::LoadOp::Load,
        "the plane persists from frame 1; clearing it here is exactly the defect: {ops:?}"
    );
}

/// The same rule for stencil: a stencil clear alone must give the pass a stencil-carrying format.
#[test]
fn a_stencil_clear_alone_materializes_a_stencil_capable_plane() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_stencil(&mut c, 3);
    record::clear_buffers(&mut c, GL_STENCIL_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let (_, stencil) = pass_depth_clear(&sink.batches[0]).expect("a stencil-capable attachment");
    assert_eq!(stencil, 3);
}

/// And the common case stays exactly as it was: a frame with neither a depth/stencil clear nor a
/// depth/stencil-tested draw carries NO depth attachment at all.
#[test]
fn a_plain_colour_frame_still_carries_no_depth_attachment() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_color(&mut c, [0.0, 0.25, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(
        pass_depth_clear(&sink.batches[0]).is_none(),
        "the 2D path must not grow a depth plane"
    );
}

/// The DISCRIMINATOR shape: a depth clear, a readback (frame boundary) with NO intervening draw, then a
/// depth-tested draw. This separates "the intervening draw is the trigger" from "the frame boundary is the
/// trigger" — and it is the frame boundary. A clear-only frame used to lower to no frame at all, so the
/// cleared depth never reached a plane and the next frame minted one at the GL initial 1.0.
#[test]
fn a_depth_clear_alone_survives_a_frame_boundary() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);

    // Frame 1: nothing but the depth clear.
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    assert!(
        swap::swap_buffers(&mut c, &mut sink).unwrap(),
        "a depth clear is work, even with no draw and no colour clear"
    );
    let first = pass_depth_clear(&sink.batches[0]).expect("the depth plane the clear names");
    assert_eq!(first.0, 0.5);

    // Frame 2: the dependent depth-tested draw. The plane already exists, so it load-preserves 0.5.
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_func(&mut c, GL_LESS);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[1]);
    let load = ops
        .iter()
        .find_map(|e| match e {
            Enc::BeginRenderPass { depth, .. } => depth.as_ref().map(|d| d.load),
            _ => None,
        })
        .expect("the depth attachment");
    assert_eq!(
        load,
        hl_gpu::protocol::model::enums::LoadOp::Load,
        "clearing here would discard the 0.5 and let GL_LESS pass: {ops:?}"
    );
}

/// The clear matrix, as the differential established it: depth, stencil, and the STENCIL HALF of a
/// combined clear were all lost at a frame boundary, while the colour clear inside the same `glClear`
/// always survived.
///
/// The mechanism is one line in `swap::flush`: when the frame builder returns nothing, the recording is
/// `reset_frame()`d anyway, so a clear that lowered to no frame was discarded rather than retained. Every
/// boundary that loses it — `glFlush`, `glFinish`, `glReadPixels` — funnels through that same builder,
/// which is why a fix confined to the readback path would have left the other two broken. A colour clear
/// always built a frame, so it never took that branch.
///
/// The combined case has its own trap: the pass format was chosen from the draws alone, so a frame with a
/// depth-tested draw took `Depth32Float` and the stencil half of the clear had no plane to land on — depth
/// survived and stencil did not, in the same call.
#[test]
fn a_combined_depth_stencil_clear_keeps_both_halves() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_depth(&mut c, 0.25);
    record::clear_stencil(&mut c, 7);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    // A DEPTH-tested (but not stencil-tested) draw: the pass would otherwise take a depth-only format and
    // silently drop the stencil half of the clear above.
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());

    let (depth, stencil) = pass_depth_clear(&sink.batches[0]).expect("a depth/stencil attachment");
    assert_eq!(depth, 0.25, "the depth half");
    assert_eq!(stencil, 7, "and the stencil half, in the same clear");
}

/// A stencil-only clear with no stencil-tested draw anywhere still has to land.
#[test]
fn a_stencil_only_clear_lands_without_a_stencil_tested_draw() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_stencil(&mut c, 5);
    record::clear_buffers(&mut c, GL_STENCIL_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let (_, stencil) = pass_depth_clear(&sink.batches[0]).expect("a stencil-capable attachment");
    assert_eq!(stencil, 5);
}

// ---------------------------------------------------------------------------------------------------
// A clear that `LoadOp::Clear` cannot express lowers to a scissored, masked rect DRAW
//
// The claim these refute is that a channel-masked clear is "not representable in the IR". It is false,
// and it is stated in FIVE places, one of which is a runtime diagnostic rather than a comment. Whoever
// implements the fix must delete all five, because each one on its own re-lays the trap:
//
//   1. `model/program/draw.rs`  — "a PARTIALLY masked color clear … is not representable by
//                                  `LoadOp::Clear` or `Enc::ClearRect`"
//   2. `model/program/draw.rs`  — "unrepresentable, so refused" on `color_clear_is_partial`
//   3. `service/record/draw.rs` — "Neither `LoadOp::Clear` nor `Enc::ClearRect` can express that"
//   4. `service/record/draw.rs` — the `hl_error!` STRING: "a channel-masked clear is not representable
//                                  in the IR", emitted once per context, to the operator, at error level
//   5. `service/record/state.rs`— "refused on the same terms as `glClear`'s", which propagates the claim
//                                  to `glClearBuffer*` by citation rather than by re-derivation
//
// `service/frame/geometry.rs`'s "only the clears it genuinely cannot express" is about ROUTING, is
// correct, and must not be deleted with them.
//
// What is actually available, read out of `Enc` and `RenderPipelineDesc`: `SetScissor` bounds the rect;
// a viewport whose depth range is collapsed to one value writes that exact depth from any geometry;
// `SetStencilReference` with a `REPLACE` pass op writes the stencil value; `SetBlendConstant` with
// `src_factor = CONSTANT, dst_factor = ZERO` writes the colour whatever the fragment emits; and the
// pipeline's `stencil_write_mask` / `write_mask` carry `glStencilMask` and `glColorMask` exactly. The
// two value tricks are what keep this to ONE static shader pair rather than one per clear value.
//
// `Enc::ClearRect` really cannot be extended to cover it, for a reason the claim never gave: it is a
// processor-side texel write through `Queue::write_texture`, invalid for a depth texture.
//
// Full write-up: `hl-work/gl-clear-scissor-20260731/{DESIGN,FINDING}.md`.
// ---------------------------------------------------------------------------------------------------

/// The depth attachment's load op, and whether any pass in the frame carries one.
fn depth_load(batch: &[Cmd]) -> Option<LoadOp> {
    submit_ops(batch).iter().find_map(|e| match e {
        Enc::BeginRenderPass { depth, .. } => depth.as_ref().map(|d| d.load),
        _ => None,
    })
}

/// The depth state of the first render pipeline created in the batch.
fn pipeline_depth(batch: &[Cmd]) -> Option<hl_gpu::protocol::model::descriptor::DepthState> {
    batch.iter().find_map(|c| match c {
        Cmd::CreateRenderPipeline(_, desc) | Cmd::CreateRenderPipelineLayout(_, desc, _, _) => {
            desc.depth.clone()
        }
        _ => None,
    })
}

fn pipeline_color_write_mask(batch: &[Cmd]) -> Option<u32> {
    batch.iter().find_map(|c| match c {
        Cmd::CreateRenderPipeline(_, desc) | Cmd::CreateRenderPipelineLayout(_, desc, _, _) => {
            desc.color_targets.first().map(|t| t.write_mask)
        }
        _ => None,
    })
}

fn scissors(batch: &[Cmd]) -> Vec<(u32, u32, u32, u32)> {
    submit_ops(batch)
        .iter()
        .filter_map(|e| match e {
            Enc::SetScissor { x, y, w, h } => Some((*x, *y, *w, *h)),
            _ => None,
        })
        .collect()
}

/// A SCISSORED depth clear must paint only its rect. Clear-loading the depth attachment wipes the whole
/// plane, which is indistinguishable from ignoring `GL_SCISSOR_TEST` — and `glClear` is scissor-tested
/// for every plane it names, not just colour.
#[test]
fn a_scissored_depth_clear_does_not_clear_load_the_whole_attachment() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::depth_mask(&mut c, true);

    // Establish the depth plane in an earlier frame. A depth texture minted THIS frame has no prior
    // contents and always clear-loads to the GL initial 1.0 regardless of the caller's op (see
    // `depth_attachment_for`), which would mask the property under test — this assertion is about the
    // clear's own value never reaching the whole attachment, not about first-use initialization.
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    sink.batches.clear();

    record::clear_depth(&mut c, 0.25);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [8, 8, 16, 16]);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    record::disable(&mut c, GL_SCISSOR_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    assert_eq!(
        depth_load(&sink.batches[0]),
        Some(LoadOp::Load),
        "a scissored depth clear must not clear-load the whole depth attachment"
    );
    // GL window rows convert to texture rows, so the box at y = 8 height 16 on a 256-row target is at
    // top row 256 - 8 - 16 = 232.
    assert!(
        scissors(&sink.batches[0]).contains(&(8, 232, 16, 16)),
        "the clear's rect must reach the encoder: {:?}",
        scissors(&sink.batches[0])
    );
    // And the clear VALUE rides the collapsed viewport depth range, which is what writes it inside that
    // rect without touching anything outside it.
    assert!(
        submit_ops(&sink.batches[0]).iter().any(|e| matches!(
            e,
            Enc::SetViewport { min_depth, max_depth, .. } if *min_depth == 0.25 && *max_depth == 0.25
        )),
        "the collapsed depth range carries the clear value: {:?}",
        submit_ops(&sink.batches[0])
    );
}

/// GL applies `glStencilMask` to a clear exactly as to a draw: only the enabled BITS change. Treating any
/// non-zero mask as licence to write all eight is a different image whenever an application packs more
/// than one thing into the stencil plane, which is the reason to have a mask at all.
#[test]
fn a_partially_masked_stencil_clear_writes_only_the_masked_bits() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_stencil(&mut c, 0xff);
    record::stencil_mask(&mut c, 0x0f);
    record::clear_buffers(&mut c, GL_STENCIL_BUFFER_BIT);
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    let depth = pipeline_depth(&sink.batches[0])
        .expect("a partially masked stencil clear lowers to a depth-stencil pipeline");
    assert_eq!(
        depth.stencil_write_mask & 0xff,
        0x0f,
        "the pipeline must carry glStencilMask, not a full-plane write"
    );
}

/// A partially `glColorMask`ed clear was REFUSED — the colour plane was left untouched and the whole
/// clear dropped, reported once per context as unrepresentable. `glColorMask(0,0,0,1); glClear(...)` is
/// what a browser compositor does on every frame, so this was silently unsupported throughout.
#[test]
fn a_partially_masked_colour_clear_writes_the_enabled_channels() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_color(&mut c, [1.0, 1.0, 1.0, 1.0]);
    record::color_mask(&mut c, false, false, false, true);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    assert_eq!(
        pipeline_color_write_mask(&sink.batches[0]),
        Some(0b1000),
        "only the alpha channel may be written"
    );
}

// ---------------------------------------------------------------------------------------------------
// The fast path is the point: the rect draw is a FALLBACK, not a replacement
// ---------------------------------------------------------------------------------------------------

/// Whether the batch created the internal clear pipeline (the rect-draw fallback).
fn used_rect_clear(batch: &[Cmd]) -> bool {
    batch.iter().any(|c| match c {
        Cmd::CreateRenderPipeline(_, desc) | Cmd::CreateRenderPipelineLayout(_, desc, _, _) => {
            desc.label == "gl-rect-clear"
        }
        _ => false,
    })
}

/// An UNSCISSORED, UNMASKED clear must keep lowering to a single attachment load op. That is one
/// operation against a full-target draw, it is the fast path on tiled hardware, and it is what the
/// overwhelming majority of frames do — the rect draw exists only for what a load op cannot express.
#[test]
fn an_ordinary_clear_still_lowers_to_a_load_op_and_no_draw() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(
        &mut c,
        GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT,
    );
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    let ops = submit_ops(&sink.batches[0]);
    assert!(
        !used_rect_clear(&sink.batches[0]),
        "an ordinary clear must not take the rect-draw fallback: {ops:?}"
    );
    match &ops[0] {
        Enc::BeginRenderPass { color, depth } => {
            assert_eq!(color[0].load, LoadOp::Clear, "colour clear-loads");
            assert_eq!(color[0].clear, [0.25, 0.5, 0.75, 1.0]);
            let depth = depth.as_ref().expect("the depth attachment");
            assert_eq!(depth.load, LoadOp::Clear);
            assert_eq!(depth.clear_depth, 0.5);
        }
        other => panic!("expected the pass to open the frame, got {other:?}"),
    }
}

/// A scissor box that covers the complete draw target is observably identical to no scissor for a clear.
/// dEQP uses this exact state in every rasterizer-discard scissor case; routing it through the internal
/// rect-clear draw needlessly introduces another pipeline and bind-group boundary.
#[test]
fn a_full_target_scissor_clear_uses_attachment_loads() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [0, 0, 256, 256]);
    record::clear_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    record::clear_depth(&mut c, 0.5);
    record::clear_stencil(&mut c, 0);
    record::clear_buffers(
        &mut c,
        GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT,
    );
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    let ops = submit_ops(&sink.batches[0]);
    assert!(
        !used_rect_clear(&sink.batches[0]),
        "a target-covering scissor must retain the attachment-load fast path: {ops:?}"
    );
    assert!(matches!(
        ops.first(),
        Some(Enc::BeginRenderPass { color, depth: Some(depth) })
            if color[0].load == LoadOp::Clear && depth.load == LoadOp::Clear
    ));
}

/// A SCISSORED but unmasked colour clear keeps its `Enc::ClearRect`: it already paints exactly its rect
/// with every channel enabled, and a rect FILL is cheaper than a rect DRAW.
#[test]
fn a_scissored_unmasked_colour_clear_still_uses_a_rect_fill() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [4, 4, 8, 8]);
    record::clear_color(&mut c, [1.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    assert!(
        submit_ops(&sink.batches[0])
            .iter()
            .any(|e| matches!(e, Enc::ClearRect { .. })),
        "the scissored colour clear keeps its fill"
    );
    assert!(
        !used_rect_clear(&sink.batches[0]),
        "and must not have been promoted to a draw"
    );
}

/// The internal clear shaders and pipeline are created ONCE and reused. The clear VALUES are dynamic
/// encoder state — the collapsed viewport range, the stencil reference and the blend constant — so two
/// clears differing only in value must share one pipeline.
#[test]
fn two_clears_at_different_values_share_one_pipeline() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    for value in [0.25f32, 0.75] {
        record::clear_depth(&mut c, value);
        record::enable(&mut c, GL_SCISSOR_TEST);
        record::scissor(&mut c, [4, 4, 8, 8]);
        record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
        record::disable(&mut c, GL_SCISSOR_TEST);
        record::enable(&mut c, GL_DEPTH_TEST);
        record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
        swap::swap_buffers(&mut c, &mut sink).unwrap();
    }
    let creates: usize = sink
        .batches
        .iter()
        .map(|b| {
            b.iter()
                .filter(|c| matches!(c, Cmd::CreateShader { .. }))
                .count()
        })
        .sum();
    // Two shader modules for the app's program, two for the internal clear — and nothing on the second
    // frame, whose clear value differs but whose pipeline does not.
    assert_eq!(creates, 4, "the clear shaders are created once per context");
    let clear_pipelines: usize = sink.batches.iter().filter(|b| used_rect_clear(b)).count();
    assert_eq!(clear_pipelines, 1, "and so is the pipeline");
}

// ---------------------------------------------------------------------------------------------------
// glDepthRangef reaches the viewport transform
// ---------------------------------------------------------------------------------------------------

/// `glDepthRangef` was discarded at the ABI boundary and the viewport was lowered with a hard-coded
/// `[0, 1]`, so every fragment landed at its default-range depth however the application set the range.
/// It is invisible to any test using the default range, which is why it survived — and the rect clear
/// depends on exactly this encoder field to place its fragments, so the two paths disagreed about
/// whether the range meant anything.
fn viewport_depth(batch: &[Cmd]) -> Option<(f32, f32)> {
    submit_ops(batch).iter().find_map(|e| match e {
        Enc::SetViewport {
            min_depth,
            max_depth,
            ..
        } => Some((*min_depth, *max_depth)),
        _ => None,
    })
}

#[test]
fn a_draw_carries_the_depth_range_the_application_set() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::depth_range(&mut c, 0.25, 0.75);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    assert_eq!(viewport_depth(&sink.batches[0]), Some((0.25, 0.75)));
}

/// ES 3.0 §2.12.1 clamps each component to `[0, 1]`, and `n > f` is legal — it reverses the mapping. The
/// host validates each component's range rather than their order, so a reversed range passes through.
#[test]
fn the_depth_range_is_clamped_per_component_and_may_be_reversed() {
    for (set, want) in [
        ((-1.0f32, 2.0f32), (0.0f32, 1.0f32)),
        ((1.0, 0.0), (1.0, 0.0)),
        ((0.75, 0.75), (0.75, 0.75)),
    ] {
        let mut c = ctx();
        let mut sink = RecordingSink::with_full_caps();
        setup_geometry(&mut c);
        record::depth_range(&mut c, set.0, set.1);
        record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
        swap::swap_buffers(&mut c, &mut sink).unwrap();
        assert_eq!(viewport_depth(&sink.batches[0]), Some(want), "set {set:?}");
    }
}

/// The default range must stay exactly `[0, 1]`, so every frame that never calls `glDepthRangef` lowers
/// byte-identically to before.
#[test]
fn the_default_depth_range_is_unchanged() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    assert_eq!(viewport_depth(&sink.batches[0]), Some((0.0, 1.0)));
}

/// `Enc::ClearRect` must be emitted BETWEEN render passes, never inside one.
///
/// The wgpu executor silently ignored any operation it did not recognise inside a render pass —
/// `ClearRect` among them — reporting success while the region stayed as it was minted, and the CPU
/// oracle performed it. So the two backends disagreed and every lowering test still passed, because a
/// lowering test asserts what was EMITTED and not what was executed. This asserts the structural property
/// that keeps the routing valid: the rect fill sits outside the pass, where the executor's flat op loop
/// handles it, which is why the scissored-unmasked path was not affected.
#[test]
fn a_scissored_clear_fill_is_emitted_outside_any_render_pass() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [4, 4, 8, 8]);
    record::clear_color(&mut c, [1.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    let ops = submit_ops(&sink.batches[0]);
    let mut depth = 0i32;
    let mut saw_fill = false;
    for op in ops {
        match op {
            Enc::BeginRenderPass { .. } => depth += 1,
            Enc::EndRenderPass => depth -= 1,
            Enc::ClearRect { .. } => {
                saw_fill = true;
                assert_eq!(
                    depth, 0,
                    "a ClearRect inside a pass is dropped by the wgpu executor: {ops:?}"
                );
            }
            _ => {}
        }
    }
    assert!(
        saw_fill,
        "the scissored clear must still lower to a fill: {ops:?}"
    );
    assert_eq!(depth, 0, "every pass is closed");
}

/// The general form of the rule above, over a frame rich enough to emit every kind of op this driver
/// produces: NO copy, blit, fill or resolve may appear inside a render pass.
///
/// The wgpu executor silently dropped exactly these when it found them inside a pass, and reported
/// success. The driver has always hoisted its staging copies ahead of `BeginRenderPass` and emitted its
/// blits as standalone submits, so it was not affected — but nothing asserted that, and a future change
/// that pushed one into `draw_ops` instead of `copies` would be invisible in both the tests and the
/// executor. It is one predicate over the emitted stream, so it costs nothing to hold.
#[test]
fn no_transfer_operation_is_emitted_inside_a_render_pass() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);

    // A sampled texture with a mip chain (staging copies), a scissored clear (a fill), a rect clear
    // (a draw), and ordinary geometry.
    let tex = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 4, 4, &[0x40u8; 4 * 4 * 4]);
    record::tex_image_2d_level(&mut c, 1, 2, 2, &[0x50u8; 2 * 2 * 4]);
    record::tex_image_2d_level(&mut c, 2, 1, 1, &[0x60u8; 4]);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [2, 2, 4, 4]);
    record::clear_color(&mut c, [0.0, 1.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    record::disable(&mut c, GL_SCISSOR_TEST);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();

    for batch in &sink.batches {
        for cmd in batch {
            let Cmd::Submit(buffer) = cmd else { continue };
            let mut depth = 0i32;
            for op in &buffer.encoder {
                match op {
                    Enc::BeginRenderPass { .. } => depth += 1,
                    Enc::EndRenderPass => depth -= 1,
                    Enc::ClearRect { .. }
                    | Enc::CopyBufferToTexture { .. }
                    | Enc::CopyTextureToBuffer { .. }
                    | Enc::CopyBufferToBuffer { .. }
                    | Enc::CopyTextureToTexture { .. }
                    | Enc::BlitTexture { .. }
                    | Enc::ResolveTexture { .. }
                    | Enc::FillBuffer { .. } => assert_eq!(
                        depth, 0,
                        "a transfer op inside a render pass is silently dropped by the wgpu \
                         executor: {:?}",
                        buffer.encoder
                    ),
                    _ => {}
                }
            }
            assert_eq!(depth, 0, "every pass is closed");
        }
    }
}
