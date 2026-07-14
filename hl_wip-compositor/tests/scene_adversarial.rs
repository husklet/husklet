//! Adversarial / edge-case coverage for the platform-neutral compositor policy (`scene`).
//!
//! Complements `tests/scene.rs` (the happy-path proofs) with the malformed, boundary, and invariant
//! cases the mission demands: zero/1×1/huge/off-screen/overflow geometry, out-of-order commit/attach,
//! missing/unknown surfaces, HiDPI + viewport sizing, damage bounds, z-order stability, subsurface +
//! popup linkage, popup constraint math corners, focus/hit-test edges, and frame pacing across scripted
//! clock anomalies. Everything runs against a `FakeClock` + a recording `FakePresenter` — no Smithay, no
//! GPU. Assertions target real composed output (present order, layer offsets, damage rects, pacing),
//! never "it didn't panic".

use std::cell::Cell;
use std::sync::Mutex;

use hl_compositor::scene::model::{
    Anchor, BufferState, ConstraintAdjustment, DamageRegion, Format, Gravity, Output, OutputId,
    PopupState, Positioner, PresentableImage, Rect, Scene, SubsurfaceState, Surface, SurfaceId,
    SurfaceRole, Viewport, Visibility,
};
use hl_compositor::scene::port::{
    Clock, PresentOutcome, PresentTiming, PresentationFeedback, Presenter,
};
use hl_compositor::scene::service::{
    self, commit_surface, compose_frame, focus, is_tree_dirty, place_popup, place_popup_in,
    schedule, BufferChange, Commit, FramePacing,
};
use hl_compositor::Compositor;

// ---- fakes -------------------------------------------------------------------------------------

struct FakeClock {
    now: Cell<u64>,
}
impl FakeClock {
    fn new(start: u64) -> FakeClock {
        FakeClock { now: Cell::new(start) }
    }
    fn set(&self, ns: u64) {
        self.now.set(ns);
    }
}
impl Clock for FakeClock {
    fn now_nanos(&self) -> u64 {
        self.now.get()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PresentCall {
    output: OutputId,
    surface: SurfaceId,
    width: i32,
    height: i32,
    damage: Vec<Rect>,
}

struct FakePresenter {
    calls: Mutex<Vec<PresentCall>>,
    next_serial: Cell<u64>,
    scripted: Mutex<Vec<PresentOutcome>>,
}
impl FakePresenter {
    fn new() -> FakePresenter {
        FakePresenter {
            calls: Mutex::new(Vec::new()),
            next_serial: Cell::new(1),
            scripted: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<PresentCall> {
        self.calls.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn script(&self, outcome: PresentOutcome) {
        self.scripted.lock().unwrap().push(outcome);
    }
}
impl Presenter for FakePresenter {
    fn present(
        &mut self,
        output: OutputId,
        image: &PresentableImage,
        damage: &[Rect],
        _timing: PresentTiming,
    ) -> PresentationFeedback {
        self.calls.lock().unwrap().push(PresentCall {
            output,
            surface: image.surface,
            width: image.width,
            height: image.height,
            damage: damage.to_vec(),
        });
        let outcome = self.scripted.lock().unwrap().pop().unwrap_or_else(|| {
            let s = self.next_serial.get();
            self.next_serial.set(s + 1);
            PresentOutcome::Delivered { serial: s, timing: None }
        });
        PresentationFeedback { outcome }
    }
}

// ---- helpers -----------------------------------------------------------------------------------

fn shm(w: i32, h: i32) -> BufferState {
    BufferState { tex_w: w, tex_h: h, format: Format::Argb8888, buffer_scale: 1, gpu: false }
}
fn xrgb(w: i32, h: i32) -> BufferState {
    BufferState { tex_w: w, tex_h: h, format: Format::Xrgb8888, buffer_scale: 1, gpu: false }
}

fn scene_with_output() -> Scene {
    let mut scene = Scene::new();
    scene.add_output(Output::new(OutputId(1), "hl-0", 2560, 1440, 60_000));
    scene
}

fn compositor() -> Compositor<FakePresenter, FakeClock> {
    let mut c = Compositor::new(FakePresenter::new(), FakeClock::new(0));
    c.scene.add_output(Output::new(OutputId(1), "hl-0", 2560, 1440, 60_000));
    c
}

fn map_toplevel(scene: &mut Scene, w: i32, h: i32) -> SurfaceId {
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    commit_surface(scene, id, Commit::attach(shm(w, h)));
    id
}

fn sub(parent: SurfaceId, x: i32, y: i32) -> SurfaceRole {
    SurfaceRole::Subsurface(SubsurfaceState { parent, x, y, sync: false })
}

fn popup_role(parent: SurfaceId, geometry: Rect) -> SurfaceRole {
    SurfaceRole::Popup(PopupState { parent, positioner: menu_positioner(), geometry, grabbed: false })
}

fn menu_positioner() -> Positioner {
    Positioner {
        anchor_rect: Rect::new(0, 0, 1, 1),
        size: (180, 240),
        anchor: Anchor::BottomLeft,
        gravity: Gravity::BottomRight,
        constraint_adjustment: ConstraintAdjustment::NONE,
        offset: (0, 0),
    }
}

// =================================================================================================
// 1. Rect geometry: the primitive every damage/occlusion/hit decision rests on
// =================================================================================================

#[test]
fn rect_empty_and_negative_sizes_cover_nothing() {
    assert!(Rect::new(0, 0, 0, 10).is_empty(), "zero width is empty");
    assert!(Rect::new(0, 0, 10, 0).is_empty(), "zero height is empty");
    assert!(Rect::new(0, 0, -5, 5).is_empty(), "negative width is empty");
    // An empty rect contains no point and no rect, and is contained by none.
    let empty = Rect::new(0, 0, 0, 0);
    assert!(!empty.contains_point(0, 0));
    assert!(!Rect::new(0, 0, 100, 100).contains_rect(&empty), "empty target is never 'contained'");
    assert!(!empty.contains_rect(&Rect::new(0, 0, 1, 1)));
    assert!(!empty.intersects(&Rect::new(-5, -5, 100, 100)));
}

#[test]
fn rect_contains_point_is_half_open() {
    let r = Rect::new(10, 20, 30, 40); // x:[10,40), y:[20,60)
    assert!(r.contains_point(10, 20), "top-left inclusive");
    assert!(r.contains_point(39, 59), "just inside the far edge");
    assert!(!r.contains_point(40, 59), "right edge exclusive");
    assert!(!r.contains_point(39, 60), "bottom edge exclusive");
    assert!(!r.contains_point(9, 20), "left of the rect");
    assert!(!r.contains_point(10, 19), "above the rect");
}

#[test]
fn rect_contains_rect_requires_full_containment() {
    let outer = Rect::new(0, 0, 100, 100);
    assert!(outer.contains_rect(&Rect::new(0, 0, 100, 100)), "equal rect is contained");
    assert!(outer.contains_rect(&Rect::new(10, 10, 80, 80)));
    assert!(!outer.contains_rect(&Rect::new(50, 50, 60, 10)), "spills past the right edge");
    assert!(!outer.contains_rect(&Rect::new(-1, 0, 10, 10)), "starts left of the outer");
}

#[test]
fn rect_intersects_touching_edges_do_not_overlap() {
    let a = Rect::new(0, 0, 10, 10);
    assert!(!a.intersects(&Rect::new(10, 0, 10, 10)), "edge-adjacent = no positive-area overlap");
    assert!(!a.intersects(&Rect::new(0, 10, 10, 10)));
    assert!(a.intersects(&Rect::new(9, 9, 10, 10)), "1px overlap counts");
}

#[test]
fn rect_union_ignores_empties_and_bounds_both() {
    let a = Rect::new(10, 10, 20, 20);
    let empty = Rect::new(0, 0, 0, 0);
    assert_eq!(a.union(&empty), a, "union with empty returns the non-empty side");
    assert_eq!(empty.union(&a), a);
    assert_eq!(empty.union(&empty), empty);
    let b = Rect::new(50, 5, 10, 10);
    let u = a.union(&b);
    assert_eq!(u, Rect::new(10, 5, 50, 25), "union spans both extents (x:10..60, y:5..30)");
}

#[test]
fn rect_translate_moves_origin_keeps_size() {
    let r = Rect::new(5, 6, 7, 8).translate(-10, 100);
    assert_eq!(r, Rect::new(-5, 106, 7, 8));
}

// =================================================================================================
// 2. DamageRegion accumulator
// =================================================================================================

#[test]
fn damage_region_drops_empties_and_bounds_the_rest() {
    let mut d = DamageRegion::new();
    assert!(d.is_empty());
    assert_eq!(d.bounding_box(), None, "no damage => no bounding box");
    d.add(Rect::new(0, 0, 0, 0)); // empty ignored
    d.add(Rect::new(0, 0, -3, 5)); // negative ignored
    assert!(d.is_empty(), "only empty/negative rects added => still empty");
    d.add(Rect::new(10, 10, 5, 5));
    d.add(Rect::new(100, 0, 5, 5));
    assert_eq!(d.bounding_box(), Some(Rect::new(10, 0, 95, 15)));
    d.clear();
    assert!(d.is_empty() && d.bounding_box().is_none());
}

// =================================================================================================
// 3. Output: logical size + refresh derivation, boundary scales
// =================================================================================================

#[test]
fn output_logical_size_divides_by_scale_and_clamps() {
    let o = Output::new(OutputId(1), "o", 2560, 1440, 60_000).with_scale(2);
    assert_eq!(o.logical_size(), (1280, 720));
    // A scale larger than the mode clamps each axis to >= 1 (never zero-size).
    let tiny = Output::new(OutputId(2), "o", 1, 1, 60_000).with_scale(4);
    assert_eq!(tiny.logical_size(), (1, 1));
    // with_scale never accepts < 1.
    let z = Output::new(OutputId(3), "o", 800, 600, 60_000).with_scale(0);
    assert_eq!(z.scale, 1, "scale is clamped up to 1");
}

#[test]
fn output_refresh_nanos_handles_unknown_rate() {
    assert_eq!(Output::new(OutputId(1), "o", 1, 1, 60_000).refresh_nanos(), 16_666_666);
    assert_eq!(Output::new(OutputId(1), "o", 1, 1, 0).refresh_nanos(), 0, "unknown rate => 0");
    assert_eq!(Output::new(OutputId(1), "o", 1, 1, -1).refresh_nanos(), 0, "negative rate => 0");
}

#[test]
fn scene_output_logical_size_falls_back_without_output() {
    let empty = Scene::new();
    assert_eq!(empty.output_logical_size(), (1000, 700), "no output => sane fallback");
    let scene = scene_with_output();
    assert_eq!(scene.output_logical_size(), (2560, 1440));
}

// =================================================================================================
// 4. BufferState logical size: viewport, buffer scale, HiDPI, degenerate sizes
// =================================================================================================

#[test]
fn buffer_logical_size_honours_viewport_dst_then_src_then_scale() {
    // dst wins over everything.
    let b = shm(800, 600);
    let vp_dst = Viewport { dst: Some((320, 240)), src: None };
    assert_eq!(b.logical_size(&vp_dst), (320, 240));

    // A src crop's size wins when no dst.
    let vp_src = Viewport { dst: None, src: Some((0.0, 0.0, 100.4, 50.6)) };
    assert_eq!(b.logical_size(&vp_src), (100, 51), "src size rounds to nearest, >=1");

    // Neither: tex / buffer_scale (HiDPI), clamped to >= 1.
    let hidpi = BufferState { buffer_scale: 2, ..shm(800, 600) };
    assert_eq!(hidpi.logical_size(&Viewport::default()), (400, 300));

    // A degenerate dst (0 or negative) is ignored and falls through to scale.
    let vp_bad_dst = Viewport { dst: Some((0, 240)), src: None };
    assert_eq!(b.logical_size(&vp_bad_dst), (800, 600), "zero dst dimension ignored");
}

#[test]
fn buffer_logical_size_never_zero_for_tiny_buffers() {
    // A 1×1 buffer at scale 4 must not collapse to 0×0.
    let b = BufferState { buffer_scale: 4, ..shm(1, 1) };
    assert_eq!(b.logical_size(&Viewport::default()), (1, 1));
}

#[test]
fn format_opacity_classification() {
    assert!(Format::Xrgb8888.is_opaque());
    assert!(!Format::Argb8888.is_opaque());
}

// =================================================================================================
// 5. commit_surface: malformed / out-of-order / unknown-surface paths
// =================================================================================================

#[test]
fn commit_to_unknown_surface_is_a_no_op() {
    let mut scene = scene_with_output();
    let changed = commit_surface(&mut scene, SurfaceId(999), Commit::attach(shm(10, 10)));
    assert!(!changed, "committing to a non-existent surface reports no change");
    assert!(!scene.any_dirty());
}

#[test]
fn damage_before_any_buffer_does_not_dirty() {
    // Out-of-order: damage arrives before the first attach. Nothing is visible yet.
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    let changed = commit_surface(
        &mut scene,
        id,
        Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(0, 0, 10, 10)),
    );
    assert!(!changed, "damage with no buffer is not a content change");
    assert!(!scene.is_dirty(id));
    assert!(scene.get(id).unwrap().damage.is_empty(), "no damage accumulates without a buffer");
}

#[test]
fn detach_without_prior_buffer_reports_no_change() {
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    let changed = commit_surface(
        &mut scene,
        id,
        Commit { buffer: BufferChange::Removed, ..Commit::default() },
    );
    assert!(!changed, "detaching when nothing was attached is not a change");
}

#[test]
fn empty_damage_rects_do_not_accumulate_but_new_buffer_still_dirties() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    scene.clear_dirty(id);
    scene.get_mut(id).unwrap().damage.clear();
    // Keep-buffer commit whose only damage rects are empty: no visible change.
    let changed = commit_surface(
        &mut scene,
        id,
        Commit { buffer: BufferChange::Keep, ..Commit::default() }
            .with_damage(Rect::new(0, 0, 0, 0))
            .with_damage(Rect::new(5, 5, -1, 10)),
    );
    assert!(!changed, "only-empty damage on a live buffer is not a change");
    assert!(scene.get(id).unwrap().damage.is_empty());

    // Re-attaching a buffer always dirties even if the damage list is empty.
    let changed = commit_surface(&mut scene, id, Commit::attach(shm(100, 100)));
    assert!(changed, "a fresh attach is always a content change");
    assert!(scene.is_dirty(id));
}

#[test]
fn region_and_title_only_commit_updates_state_without_dirtying() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    scene.clear_dirty(id);
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Keep,
            opaque_region: Some(Some(Rect::new(0, 0, 50, 50))),
            input_region: Some(Some(Rect::new(10, 10, 20, 20))),
            title: Some("hello".into()),
            ..Commit::default()
        },
    );
    assert!(!changed, "region/title-only commit is not a content change");
    assert!(!scene.is_dirty(id));
    let s = scene.get(id).unwrap();
    assert_eq!(s.opaque_region, Some(Rect::new(0, 0, 50, 50)));
    assert_eq!(s.input_region, Some(Rect::new(10, 10, 20, 20)));
    assert_eq!(s.title, "hello");
}

#[test]
fn opaque_region_can_be_explicitly_cleared() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    commit_surface(
        &mut scene,
        id,
        Commit { opaque_region: Some(Some(Rect::new(0, 0, 100, 100))), ..Commit::default() },
    );
    assert!(scene.get(id).unwrap().opaque_region.is_some());
    // Some(None): the client set an empty opaque region — clears it.
    commit_surface(&mut scene, id, Commit { opaque_region: Some(None), ..Commit::default() });
    assert_eq!(scene.get(id).unwrap().opaque_region, None);
}

#[test]
fn detach_clears_accumulated_damage() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    commit_surface(
        &mut scene,
        id,
        Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(0, 0, 10, 10)),
    );
    assert!(!scene.get(id).unwrap().damage.is_empty());
    commit_surface(&mut scene, id, Commit { buffer: BufferChange::Removed, ..Commit::default() });
    assert!(scene.get(id).unwrap().damage.is_empty(), "detach clears damage");
    assert!(scene.get(id).unwrap().buffer.is_none());
}

#[test]
fn frame_callbacks_accumulate_across_commits() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    for _ in 0..5 {
        commit_surface(&mut scene, id, Commit::frame_callback_only());
    }
    assert_eq!(scene.get(id).unwrap().pending_callbacks, 5);
}

// =================================================================================================
// 6. Scene tree structure: linkage, removal, id allocation
// =================================================================================================

#[test]
fn set_role_registers_subsurface_child_once_and_in_order() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let a = scene.create_surface();
    let b = scene.create_surface();
    scene.set_role(a, sub(top, 0, 0));
    scene.set_role(b, sub(top, 0, 0));
    // Re-assigning the same role must not double-register the child.
    scene.set_role(a, sub(top, 5, 5));
    assert_eq!(scene.subsurface_children(top), &[a, b], "registration order = z-order, no dup");
}

#[test]
fn remove_surface_unlinks_from_parent_and_clears_focus() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, 10, 10));
    commit_surface(&mut scene, child, Commit::attach(shm(50, 50)));
    focus::focus_surface(&mut scene, child);
    focus::update_pointer(&mut scene, top, 15.0, 15.0);
    assert_eq!(scene.seat().pointer_focus, Some(child));

    scene.remove_surface(child);
    assert!(!scene.contains(child));
    assert_eq!(scene.subsurface_children(top), &[] as &[SurfaceId], "child unlinked from parent");
    assert_eq!(scene.seat().keyboard_focus, None, "keyboard focus cleared");
    assert_eq!(scene.seat().pointer_focus, None, "pointer focus cleared");
    // Idempotent.
    scene.remove_surface(child);
}

#[test]
fn removing_a_popup_drops_it_from_the_popup_registry() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(top, Rect::new(10, 10, 100, 100)));
    commit_surface(&mut scene, popup, Commit::attach(shm(100, 100)));
    assert_eq!(scene.collect_popups_for_root(top).len(), 1);
    scene.remove_surface(popup);
    assert_eq!(scene.collect_popups_for_root(top).len(), 0, "popup gone from registry");
    assert_eq!(scene.popup_ids().count(), 0);
}

#[test]
fn insert_surface_advances_the_id_counter_past_it() {
    let mut scene = scene_with_output();
    scene.insert_surface(Surface::new(SurfaceId(50)));
    let next = scene.create_surface();
    assert!(next.0 > 50, "create_surface never re-mints an inserted id (got {next:?})");
}

#[test]
fn window_root_climbs_subsurface_then_popup_chain() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let subsurf = scene.create_surface();
    scene.set_role(subsurf, sub(top, 10, 10));
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(subsurf, Rect::new(5, 5, 50, 50)));
    let nested = scene.create_surface();
    scene.set_role(nested, sub(popup, 1, 1));

    assert_eq!(scene.window_root(nested), Some(top), "climb sub->popup->sub->top to the toplevel");
    // present_root STOPS at the popup (its own native window), not the toplevel.
    assert_eq!(scene.present_root(nested), Some(popup));
    assert_eq!(scene.present_root(subsurf), Some(top), "a subsurface resolves to its toplevel");
}

#[test]
fn window_root_survives_a_self_referential_cycle() {
    // A pathological subsurface whose parent is itself must not hang (depth guard).
    let mut scene = scene_with_output();
    let a = scene.create_surface();
    scene.set_role(a, SurfaceRole::Subsurface(SubsurfaceState { parent: a, x: 0, y: 0, sync: false }));
    // Bounded walk returns *something* rather than looping forever.
    let _ = scene.window_root(a);
    let mut out = Vec::new();
    scene.collect_tree_surfaces(a, &mut out); // self-child guarded against infinite recursion
    assert_eq!(out, vec![a]);
}

// =================================================================================================
// 7. compose_frame: geometry, damage translation, empty layers, z-order
// =================================================================================================

#[test]
fn compose_returns_none_when_root_has_no_buffer() {
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    assert!(compose_frame(&scene, id).is_none(), "no root buffer => nothing to present");
}

#[test]
fn compose_skips_a_subsurface_with_no_buffer() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 200, 200);
    let bufferless = scene.create_surface();
    scene.set_role(bufferless, sub(top, 10, 10)); // never attaches a buffer
    let real = scene.create_surface();
    scene.set_role(real, sub(top, 20, 20));
    commit_surface(&mut scene, real, Commit::attach(shm(50, 50)));

    let frame = compose_frame(&scene, top).unwrap();
    assert_eq!(frame.present_order(), vec![top, real], "the bufferless child contributes no layer");
}

#[test]
fn compose_damage_translates_partial_damage_into_root_space() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 300);
    // First present would clear dirty; here we assert compose damage directly.
    let child = scene.create_surface();
    scene.set_role(child, sub(top, 100, 50));
    commit_surface(&mut scene, child, Commit::attach(shm(80, 80)));
    // Clear the fresh-attach full damage and set a specific sub-rect on the child.
    scene.get_mut(child).unwrap().damage.clear();
    commit_surface(
        &mut scene,
        child,
        Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(5, 5, 10, 10)),
    );

    let frame = compose_frame(&scene, top).unwrap();
    let child_item = frame.items.iter().find(|i| i.image.surface == child).unwrap();
    assert_eq!(
        child_item.damage,
        vec![Rect::new(105, 55, 10, 10)],
        "child-local damage (5,5,10,10) lifts to root space by the (100,50) offset"
    );
}

#[test]
fn compose_dirty_layer_without_damage_rects_damages_its_whole_rect() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 300);
    // Fresh attach: dirty, but the damage list is empty => whole-rect damage.
    let frame = compose_frame(&scene, top).unwrap();
    let item = &frame.items[0];
    assert_eq!(item.damage, vec![Rect::new(0, 0, 400, 300)], "no explicit damage => full surface");
}

#[test]
fn compose_popup_image_carries_native_placement_and_offset() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 800, 600);
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(top, Rect::new(120, 90, 200, 150)));
    commit_surface(&mut scene, popup, Commit::attach(shm(200, 150)));

    let frame = compose_frame(&scene, top).unwrap();
    let item = frame.items.iter().find(|i| i.image.surface == popup).unwrap();
    assert_eq!((item.x, item.y), (120, 90));
    let placement = item.image.popup.expect("popup carries placement");
    assert_eq!((placement.parent, placement.x, placement.y), (top, 120, 90));
    // A plain toplevel layer never carries popup placement.
    assert_eq!(frame.items[0].image.popup, None);
}

#[test]
fn compose_image_format_reduces_to_opaque_or_alpha() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel);
    commit_surface(&mut scene, top, Commit::attach(xrgb(100, 100)));
    let frame = compose_frame(&scene, top).unwrap();
    assert_eq!(frame.items[0].image.format, Format::Xrgb8888);
    assert!(!frame.items[0].image.gpu);
}

#[test]
fn compose_carries_the_gpu_flag_through() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel);
    commit_surface(&mut scene, top, Commit::attach(BufferState { gpu: true, ..shm(100, 100) }));
    let frame = compose_frame(&scene, top).unwrap();
    assert!(frame.items[0].image.gpu, "the zero-copy GPU flag reaches the presentable image");
}

#[test]
fn deeply_nested_subsurface_offsets_accumulate() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let a = scene.create_surface();
    scene.set_role(a, sub(top, 10, 10));
    commit_surface(&mut scene, a, Commit::attach(shm(50, 50)));
    let b = scene.create_surface();
    scene.set_role(b, sub(a, 20, 30));
    commit_surface(&mut scene, b, Commit::attach(shm(20, 20)));
    let c = scene.create_surface();
    scene.set_role(c, sub(b, 5, 7));
    commit_surface(&mut scene, c, Commit::attach(shm(10, 10)));

    let frame = compose_frame(&scene, top).unwrap();
    assert_eq!(frame.present_order(), vec![top, a, b, c], "depth-first, bottom->top");
    let item = |sid| frame.items.iter().find(|i| i.image.surface == sid).unwrap();
    assert_eq!((item(b).x, item(b).y), (30, 40));
    assert_eq!((item(c).x, item(c).y), (35, 47), "c = top+a+b+c offsets summed");
}

#[test]
fn negative_subsurface_offset_is_preserved() {
    // A subsurface placed above/left of its parent (negative offset) is composed there, not clamped.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 200, 200);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, -30, -40));
    commit_surface(&mut scene, child, Commit::attach(shm(50, 50)));
    let frame = compose_frame(&scene, top).unwrap();
    let item = frame.items.iter().find(|i| i.image.surface == child).unwrap();
    assert_eq!((item.x, item.y), (-30, -40));
    assert_eq!(item.damage, vec![Rect::new(-30, -40, 50, 50)], "damage lifts into negative root space");
}

// =================================================================================================
// 8. Occlusion / is_tree_dirty — including the bufferless-cover regression
// =================================================================================================

#[test]
fn clean_tree_is_not_dirty() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    scene.clear_dirty(top);
    assert!(!is_tree_dirty(&scene, top));
}

#[test]
fn partial_opaque_cover_does_not_occlude() {
    // An opaque foreground that covers only PART of the dirty background cannot skip the present.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let bg = scene.create_surface();
    scene.set_role(bg, sub(top, 0, 0));
    commit_surface(&mut scene, bg, Commit::attach(shm(200, 200)));
    let fg = scene.create_surface();
    scene.set_role(fg, sub(top, 0, 0));
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(xrgb(100, 100)),
            opaque_region: Some(Some(Rect::new(0, 0, 100, 100))), // only covers a quarter of bg
            ..Commit::default()
        },
    );
    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);
    // Damage the bg where the fg does NOT cover it.
    c.apply_commit(
        bg,
        Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(150, 150, 10, 10)),
    );
    assert!(is_tree_dirty(&c.scene, top), "damage outside the opaque cover is visible");
}

#[test]
fn dirty_surface_with_unknown_geometry_forces_present() {
    // A dirty surface with no buffer (unknown logical size) can never be proven occluded.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let ghost = scene.create_surface();
    scene.set_role(ghost, sub(top, 0, 0));
    // Mark it dirty without giving it a buffer.
    scene.mark_dirty(ghost);
    assert!(is_tree_dirty(&scene, top), "a dirty surface of unknown size keeps the tree dirty");
}

#[test]
fn bufferless_surface_with_stale_opaque_region_does_not_occlude() {
    // REGRESSION: a surface that set an opaque region and then DETACHED its buffer draws nothing, so
    // it must not be treated as an opaque cover. Before the fix, `opaque_covers` consulted the stale
    // opaque region regardless of buffer presence and wrongly skipped the present (stale frame on
    // screen). The module's own contract is "a present is never wrongly skipped".
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let bg = scene.create_surface();
    scene.set_role(bg, sub(top, 0, 0));
    commit_surface(&mut scene, bg, Commit::attach(shm(200, 200)));
    let fg = scene.create_surface();
    scene.set_role(fg, sub(top, 0, 0));
    // fg attaches an opaque buffer fully covering bg, then detaches the buffer (opaque region persists).
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(xrgb(200, 200)),
            opaque_region: Some(Some(Rect::new(0, 0, 200, 200))),
            ..Commit::default()
        },
    );
    commit_surface(&mut scene, fg, Commit { buffer: BufferChange::Removed, ..Commit::default() });
    assert!(scene.get(fg).unwrap().buffer.is_none());
    assert!(scene.get(fg).unwrap().opaque_region.is_some(), "opaque region survives detach");

    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);
    assert!(!c.scene.any_dirty(), "the initial present clears the tree");

    // Damage the (now un-covered) background: it IS visible because the cover draws nothing.
    c.apply_commit(
        bg,
        Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(10, 10, 20, 20)),
    );
    assert!(
        is_tree_dirty(&c.scene, top),
        "a detached (bufferless) surface must not occlude the damage below it"
    );
}

#[test]
fn occluded_then_uncovered_forces_a_repaint() {
    // When the opaque cover shrinks so it no longer contains the dirty rect, the present resumes.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let bg = scene.create_surface();
    scene.set_role(bg, sub(top, 0, 0));
    commit_surface(&mut scene, bg, Commit::attach(shm(100, 100)));
    let fg = scene.create_surface();
    scene.set_role(fg, sub(top, 0, 0));
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(xrgb(100, 100)),
            opaque_region: Some(Some(Rect::new(0, 0, 100, 100))),
            ..Commit::default()
        },
    );
    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);

    // Fully covered damage: skipped.
    c.apply_commit(bg, Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(0, 0, 50, 50)));
    assert!(!is_tree_dirty(&c.scene, top));

    // Shrink the opaque region; the same damage is now visible.
    c.apply_commit(fg, Commit { opaque_region: Some(Some(Rect::new(0, 0, 10, 10))), ..Commit::default() });
    assert!(is_tree_dirty(&c.scene, top), "shrunken cover no longer occludes the damage");
}

// =================================================================================================
// 9. Popup placement math: corners the happy-path test doesn't reach
// =================================================================================================

#[test]
fn popup_anchor_center_and_gravity_center() {
    let p = Positioner {
        anchor_rect: Rect::new(100, 100, 40, 20),
        size: (10, 10),
        anchor: Anchor::None, // center of the anchor rect => (120, 110)
        gravity: Gravity::None, // popup centered on the anchor point
        constraint_adjustment: ConstraintAdjustment::NONE,
        offset: (0, 0),
    };
    // top-left = anchor(120,110) - half size(5,5) = (115, 105)
    assert_eq!(place_popup(&p, 2000, 2000), Rect::new(115, 105, 10, 10));
}

#[test]
fn popup_all_gravities_grow_away_from_anchor() {
    let base = Positioner {
        anchor_rect: Rect::new(500, 500, 0, 0), // a point anchor at (500,500)
        size: (100, 60),
        anchor: Anchor::None,
        gravity: Gravity::None,
        constraint_adjustment: ConstraintAdjustment::NONE,
        offset: (0, 0),
    };
    let at = |g: Gravity| {
        let geo = place_popup(&Positioner { gravity: g, ..base }, 4000, 4000);
        (geo.x, geo.y)
    };
    assert_eq!(at(Gravity::BottomRight), (500, 500), "grows down-right from the anchor");
    assert_eq!(at(Gravity::TopLeft), (400, 440), "grows up-left (origin shifts by -w,-h)");
    assert_eq!(at(Gravity::Top), (450, 440), "up: centered x, origin y - h");
    assert_eq!(at(Gravity::Left), (400, 470));
    assert_eq!(at(Gravity::BottomLeft), (400, 500));
    assert_eq!(at(Gravity::TopRight), (500, 440));
}

#[test]
fn popup_flip_x_mirrors_when_it_helps() {
    // Anchored at the right edge growing right (overflow); flip_x mirrors to grow left and fits.
    let p = Positioner {
        anchor_rect: Rect::new(480, 10, 10, 10),
        size: (100, 50),
        anchor: Anchor::Right, // anchor point x = 490
        gravity: Gravity::Right, // origin x = 490 -> right = 590 > 500
        constraint_adjustment: ConstraintAdjustment { flip_x: true, ..ConstraintAdjustment::NONE },
        offset: (0, 0),
    };
    let geo = place_popup(&p, 500, 400);
    // Flipped: anchor Left (x=480), gravity Left (origin x = 480 - 100 = 380).
    assert_eq!(geo.x, 380, "flip_x mirrors the popup back on-screen");
    assert!(geo.x >= 0 && geo.right() <= 500);
}

#[test]
fn popup_flip_is_declined_when_it_would_not_help() {
    // Overflow on the right, but the flipped placement ALSO overflows (left of 0): flip is not applied,
    // and with no slide/resize the popup stays at its unconstrained (overflowing) position.
    let p = Positioner {
        anchor_rect: Rect::new(50, 10, 10, 10),
        size: (600, 50), // wider than the 500 target on either side
        anchor: Anchor::Right, // x = 60
        gravity: Gravity::Right, // origin 60, right 660 > 500 (overflow right)
        constraint_adjustment: ConstraintAdjustment { flip_x: true, ..ConstraintAdjustment::NONE },
        offset: (0, 0),
    };
    let geo = place_popup(&p, 500, 400);
    // Flipped would put origin at 60 - 600 = -540 (overflow left), so flip is declined; origin stays 60.
    assert_eq!(geo.x, 60, "a flip that doesn't help is not applied");
}

#[test]
fn popup_slide_pushes_in_from_the_far_edge() {
    let p = Positioner {
        anchor_rect: Rect::new(10, 380, 20, 20),
        size: (100, 100),
        anchor: Anchor::Bottom, // y = 400
        gravity: Gravity::Bottom, // origin y = 400, bottom = 500 > 400
        constraint_adjustment: ConstraintAdjustment { slide_y: true, ..ConstraintAdjustment::NONE },
        offset: (0, 0),
    };
    let geo = place_popup(&p, 500, 400);
    assert_eq!(geo.bottom(), 400, "slide_y flushes the popup to the bottom edge");
    assert_eq!(geo.h, 100, "slide preserves size");
}

#[test]
fn popup_slide_of_oversized_popup_flushes_near_edge() {
    // A popup taller than the whole target, slid, ends flush with the NEAR (top) edge per the spec.
    let p = Positioner {
        anchor_rect: Rect::new(10, 300, 20, 20),
        size: (50, 900),
        anchor: Anchor::Bottom, // y = 320
        gravity: Gravity::Bottom, // origin y = 320
        constraint_adjustment: ConstraintAdjustment { slide_y: true, ..ConstraintAdjustment::NONE },
        offset: (0, 0),
    };
    let geo = place_popup_in(&p, Rect::new(0, 0, 500, 400));
    assert_eq!(geo.y, 0, "oversized popup slides flush to the near edge");
    assert_eq!(geo.h, 900, "slide never resizes");
}

#[test]
fn popup_resize_clamps_both_origin_and_extent() {
    // Origin left of the target AND extent past the right: resize clips both sides.
    let p = Positioner {
        anchor_rect: Rect::new(0, 0, 0, 0),
        size: (200, 30),
        anchor: Anchor::None,
        gravity: Gravity::Right, // origin x = 0
        constraint_adjustment: ConstraintAdjustment { resize_x: true, ..ConstraintAdjustment::NONE },
        offset: (-50, 0), // pushes origin to -50 (left overflow)
    };
    let geo = place_popup_in(&p, Rect::new(0, 0, 100, 400));
    assert_eq!(geo.x, 0, "origin clipped to the target left");
    assert!(geo.right() <= 100, "extent clipped to the target right");
    assert!(geo.w >= 1, "resize keeps a positive width");
}

#[test]
fn popup_resize_never_produces_zero_size() {
    // A popup entirely off the right edge, resize-only: width floors at 1 rather than going <= 0.
    let p = Positioner {
        anchor_rect: Rect::new(100, 0, 0, 0),
        size: (50, 50),
        anchor: Anchor::None,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment { resize_x: true, ..ConstraintAdjustment::NONE },
        offset: (0, 0),
    };
    let geo = place_popup_in(&p, Rect::new(0, 0, 100, 100));
    assert!(geo.w >= 1, "width never collapses to zero (got {})", geo.w);
}

#[test]
fn popup_flip_then_slide_ordering() {
    // flip is tried first; if it fixes the axis, slide is not applied. Here flip alone fits.
    let p = Positioner {
        anchor_rect: Rect::new(470, 10, 10, 10),
        size: (100, 50),
        anchor: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment {
            flip_x: true,
            slide_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = place_popup(&p, 500, 400);
    // Flip puts origin at 470 - 100 = 370 (fits). Slide would have flushed to right=500 (origin 400).
    assert_eq!(geo.x, 370, "flip wins over slide when it resolves the overflow");
}

#[test]
fn constrain_popup_uses_scene_output_area() {
    let scene = scene_with_output(); // 2560x1440
    let p = Positioner {
        anchor_rect: Rect::new(2550, 10, 4, 4),
        size: (100, 50),
        anchor: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment { slide_x: true, ..ConstraintAdjustment::NONE },
        offset: (0, 0),
    };
    let geo = service::constrain_popup(&scene, &p);
    assert_eq!(geo.right(), 2560, "constrained against the 2560-wide output");
}

// =================================================================================================
// 10. popup_placement / offset_to_toplevel corners
// =================================================================================================

#[test]
fn popup_placement_none_for_non_popup() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    assert_eq!(service::popup_placement(&scene, top), None, "a toplevel has no popup placement");
}

#[test]
fn popup_on_subsurface_resolves_to_the_toplevel() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let subsurf = scene.create_surface();
    scene.set_role(subsurf, sub(top, 40, 60));
    commit_surface(&mut scene, subsurf, Commit::attach(shm(100, 100)));
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(subsurf, Rect::new(5, 7, 30, 30)));
    commit_surface(&mut scene, popup, Commit::attach(shm(30, 30)));

    // popup_offset_to_toplevel resolves through the subsurface to the toplevel; the popup's own
    // geometry origin is relative to the parent's window geometry (the offset does not add the
    // subsurface position, matching the ported walk).
    let (tl, x, y, depth) = scene.popup_offset_to_toplevel(popup).unwrap();
    assert_eq!(tl, top);
    assert_eq!((x, y), (5, 7));
    assert_eq!(depth, 1);
    let popups = scene.collect_popups_for_root(top);
    assert_eq!(popups, vec![(popup, 5, 7)]);
}

#[test]
fn nested_submenu_depth_orders_parents_before_children() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 800, 600);
    let menu = scene.create_surface();
    scene.set_role(menu, popup_role(top, Rect::new(100, 50, 200, 300)));
    commit_surface(&mut scene, menu, Commit::attach(shm(200, 300)));
    let submenu = scene.create_surface();
    scene.set_role(submenu, popup_role(menu, Rect::new(60, 30, 150, 250)));
    commit_surface(&mut scene, submenu, Commit::attach(shm(150, 250)));

    let popups = scene.collect_popups_for_root(top);
    assert_eq!(popups[0].0, menu, "the menu (depth 1) is ordered before the submenu (depth 2)");
    assert_eq!(popups[1], (submenu, 160, 80), "submenu offset = menu origin + submenu origin");
}

// =================================================================================================
// 11. Focus + hit-testing edges
// =================================================================================================

#[test]
fn hit_test_respects_input_region() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel);
    commit_surface(
        &mut scene,
        top,
        Commit { input_region: Some(Some(Rect::new(50, 50, 100, 100))), ..Commit::attach(shm(300, 300)) },
    );
    // Inside the surface but OUTSIDE the input region => no hit.
    assert_eq!(focus::surface_at(&scene, top, 10, 10), None, "outside the input region rejects input");
    // Inside the input region => hit at the root offset.
    assert_eq!(focus::surface_at(&scene, top, 60, 60), Some((top, 0, 0)));
}

#[test]
fn hit_test_returns_topmost_of_overlapping_subsurfaces() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let lower = scene.create_surface();
    scene.set_role(lower, sub(top, 0, 0));
    commit_surface(&mut scene, lower, Commit::attach(shm(200, 200)));
    let upper = scene.create_surface();
    scene.set_role(upper, sub(top, 0, 0));
    commit_surface(&mut scene, upper, Commit::attach(shm(200, 200)));
    // Both cover (50,50); the later-registered (upper) is topmost.
    let hit = focus::surface_at(&scene, top, 50, 50).map(|(s, _, _)| s);
    assert_eq!(hit, Some(upper), "topmost overlapping subsurface wins");
}

#[test]
fn hit_test_misses_outside_the_whole_tree() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    assert_eq!(focus::surface_at(&scene, top, 500, 500), None, "point outside every surface => miss");
}

#[test]
fn hit_test_bufferless_surface_accepts_nothing() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel); // no buffer
    assert_eq!(focus::surface_at(&scene, top, 0, 0), None, "no buffer => no input surface");
}

#[test]
fn update_pointer_floors_fractional_coordinates() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, 50, 50));
    commit_surface(&mut scene, child, Commit::attach(shm(10, 10)));
    // 49.9 floors to 49 => outside the child (starts at 50) => hits root.
    assert_eq!(focus::update_pointer(&mut scene, top, 49.9, 49.9), Some(top));
    assert_eq!(scene.seat().pointer_location, (49.9, 49.9), "the exact fractional location is recorded");
    // 50.0 => inside the child.
    assert_eq!(focus::update_pointer(&mut scene, top, 50.0, 50.0), Some(child));
}

#[test]
fn focus_clear_and_refocus_report_changes() {
    let mut scene = scene_with_output();
    let a = map_toplevel(&mut scene, 100, 100);
    focus::focus_surface(&mut scene, a);
    let ch = focus::clear_focus(&mut scene);
    assert_eq!((ch.previous, ch.current), (Some(a), None));
    assert!(ch.changed());
    // Clearing again is a no-op change.
    let ch = focus::clear_focus(&mut scene);
    assert!(!ch.changed());
}

#[test]
fn refocusing_the_same_surface_is_not_a_change() {
    let mut scene = scene_with_output();
    let a = map_toplevel(&mut scene, 100, 100);
    focus::focus_surface(&mut scene, a);
    let ch = focus::focus_surface(&mut scene, a);
    assert!(!ch.changed(), "focusing the already-focused surface reports no change");
}

// =================================================================================================
// 12. schedule / pacing across scripted clock anomalies
// =================================================================================================

#[test]
fn should_present_treats_backward_clock_as_not_due() {
    let refresh = 16_666_666u64;
    // now < last (clock anomaly): not due (avoids a spurious present).
    assert!(!schedule::should_present(100, Some(1_000_000), refresh));
    // Exactly one interval: due.
    assert!(schedule::should_present(refresh, Some(0), refresh));
    // First present (last None): always due, even mid-interval.
    assert!(schedule::should_present(5, None, refresh));
}

#[test]
fn outcome_to_pacing_mapping_is_total() {
    use FramePacing::*;
    assert_eq!(service::from_outcome(PresentOutcome::Delivered { serial: 1, timing: None }), Presented);
    assert_eq!(service::from_outcome(PresentOutcome::Offscreen), RetryableFailure);
    assert_eq!(service::from_outcome(PresentOutcome::RetryableFailure), RetryableFailure);
    assert_eq!(service::from_outcome(PresentOutcome::TerminalFailure), TerminalFailure);
}

#[test]
fn fallback_timing_sets_vsync_only_with_a_known_refresh() {
    let t = schedule::fallback_timing(1_000, 16_666_666);
    assert!(t.vsync && t.refresh_ns == 16_666_666 && t.present_ns == 1_000);
    let t0 = schedule::fallback_timing(1_000, 0);
    assert!(!t0.vsync, "unknown refresh => no vsync claim");
}

#[test]
fn no_output_root_is_a_terminal_failure() {
    // A scene with NO output: present cannot target anything.
    let mut c = Compositor::new(FakePresenter::new(), FakeClock::new(0));
    let top = map_toplevel(&mut c.scene, 100, 100);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::TerminalFailure);
    assert_eq!(c.presenter().count(), 0);
    assert!(out.presented.is_empty());
}

#[test]
fn unknown_refresh_output_presents_every_dirty_commit() {
    // refresh_mhz = 0 => no throttle: each dirty commit presents.
    let mut c = Compositor::new(FakePresenter::new(), FakeClock::new(0));
    c.scene.add_output(Output::new(OutputId(1), "vrr", 800, 600, 0));
    let top = map_toplevel(&mut c.scene, 800, 600);
    c.present_root(top);
    assert_eq!(c.presenter().count(), 1);
    // Same clock time, new damage: still presents (no interval to wait for).
    c.apply_commit(top, Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(0, 0, 5, 5)));
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(c.presenter().count(), 2, "unknown-refresh output never throttles");
}

#[test]
fn terminal_failure_drops_retained_callbacks() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 100, 100);
    // Retain a callback via a retryable failure.
    c.presenter_mut().script(PresentOutcome::RetryableFailure);
    let mut attach = Commit::attach(shm(100, 100));
    attach.frame_callback = true;
    c.commit(top, attach);
    assert_eq!(c.retained_callbacks(top), 1);
    // Now a terminal failure: retained callbacks are dropped, not fired.
    c.clock().set(1_000_000_000);
    c.presenter_mut().script(PresentOutcome::TerminalFailure);
    c.apply_commit(top, Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(0, 0, 5, 5)));
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::TerminalFailure);
    assert_eq!(out.callbacks_fired, 0, "terminal failure fires nothing");
    assert_eq!(c.retained_callbacks(top), 0, "terminal failure drops the retained queue");
}

#[test]
fn retained_callbacks_are_bounded() {
    // A permanently-dead presenter must not grow the retained-callback queue without bound.
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 100, 100);
    for i in 0..50u64 {
        c.presenter_mut().script(PresentOutcome::RetryableFailure);
        c.clock().set(i * 1_000_000_000);
        let mut attach = Commit::attach(shm(100, 100));
        attach.frame_callback = true;
        c.commit(top, attach);
    }
    assert!(c.retained_callbacks(top) <= 16, "retained callbacks capped at MAX_RETAINED_CALLBACKS");
}

#[test]
fn cursor_commit_never_presents() {
    let mut c = compositor();
    let cursor = c.scene.create_surface();
    c.scene.set_role(cursor, SurfaceRole::Cursor);
    let out = c.commit(cursor, Commit::attach(shm(24, 24)));
    assert!(out.changed, "the cursor buffer is a content change");
    assert!(out.frame.is_none(), "a cursor is never presented as a window");
    assert_eq!(c.presenter().count(), 0);
}

#[test]
fn synchronized_subsurface_commit_does_not_present_on_its_own() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 200, 200);
    c.present_root(top);
    let n = c.presenter().count();
    let child = c.scene.create_surface();
    c.scene.set_role(child, SurfaceRole::Subsurface(SubsurfaceState { parent: top, x: 0, y: 0, sync: true }));
    let out = c.commit(child, Commit::attach(shm(50, 50)));
    assert!(out.frame.is_none(), "a synchronized subsurface presents with its parent, not alone");
    assert_eq!(c.presenter().count(), n, "no present triggered by the sync-subsurface commit");
}

#[test]
fn desync_subsurface_commit_presents_the_toplevel_tree() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 200, 200);
    c.present_root(top);
    c.clock().set(1_000_000_000);
    let child = c.scene.create_surface();
    c.scene.set_role(child, sub(top, 10, 10)); // desync
    let out = c.commit(child, Commit::attach(shm(50, 50))).frame.expect("a desync child presents");
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(out.presented, vec![top, child], "the whole toplevel tree presents");
}

#[test]
fn present_carries_coalesced_damage_bounding_box() {
    // Two damage rects within one interval coalesce; the present carries their bounding box.
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 400, 400);
    c.present_root(top); // clears the fresh-attach damage
    let refresh = c.scene.primary_output().unwrap().refresh_nanos();
    c.apply_commit(top, Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(10, 10, 20, 20)));
    c.apply_commit(top, Commit { buffer: BufferChange::Keep, ..Commit::default() }.with_damage(Rect::new(100, 100, 5, 5)));
    c.clock().set(refresh);
    c.present_root(top);
    let call = c.presenter().calls().pop().unwrap();
    assert_eq!(call.damage, vec![Rect::new(10, 10, 95, 95)], "coalesced damage bounding box presented");
}

#[test]
fn present_records_output_and_logical_dimensions() {
    let mut c = compositor();
    let top = scene_with_output_toplevel(&mut c);
    c.present_root(top);
    let call = &c.presenter().calls()[0];
    assert_eq!(call.output, OutputId(1));
    assert_eq!((call.width, call.height), (640, 480));
}

fn scene_with_output_toplevel(c: &mut Compositor<FakePresenter, FakeClock>) -> SurfaceId {
    map_toplevel(&mut c.scene, 640, 480)
}

#[test]
fn minimized_then_visible_resumes_presenting() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 200, 200);
    c.scene.set_visibility(top, Visibility::Occluded);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::RetryableFailure);
    assert_eq!(c.presenter().count(), 0, "an occluded root never reaches the presenter");
    // Reveal it: the retained dirty frame now presents.
    c.scene.set_visibility(top, Visibility::Visible);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(c.presenter().count(), 1);
}

#[test]
fn visibility_allows_present_only_when_visible() {
    assert!(Visibility::Visible.allows_present());
    assert!(!Visibility::Occluded.allows_present());
    assert!(!Visibility::Minimized.allows_present());
}
