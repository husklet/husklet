//! Fake-driven tests for the platform-neutral compositor policy.
//!
//! Everything runs against a `FakeClock` (scripted nanos) and a `FakePresenter` (records `present()`
//! calls) — NO Smithay, NO GPU, NO Cocoa/DRM. This proves the `scene` brain is a self-contained,
//! deterministic policy: commit → damage, tree compose order + damage, popup placement math, frame
//! pacing/scheduling across scripted clock times, and focus on window raise/close.

use std::cell::Cell;
use std::sync::Mutex;

use hl_compositor::scene::model::{
    Anchor, BufferState, ConstraintAdjustment, Format, Gravity, Output, OutputId, Positioner, Rect,
    Scene, SubsurfaceState, SurfaceRole, Visibility, WindowKind,
};
use hl_compositor::scene::model::{PresentableImage, SurfaceId};
use hl_compositor::scene::port::{
    Clock, PresentOutcome, PresentTiming, PresentationFeedback, Presenter,
};
use hl_compositor::scene::service::{
    commit_surface, focus, schedule, BufferChange, Commit, FramePacing,
};
use hl_compositor::Compositor;

// ---- fakes -------------------------------------------------------------------------------------

/// A clock that returns whatever nanos the test scripts (mutable via [`FakeClock::set`]).
struct FakeClock {
    now: Cell<u64>,
}

#[test]
fn window_state_is_an_authoritative_snapshot() {
    let mut scene = scene_with_output();
    let parent = map_toplevel(&mut scene, 800, 600);
    let dialog = map_toplevel(&mut scene, 420, 180);
    let surface = scene.get_mut(dialog).unwrap();
    surface.title = "Confirm".into();
    surface.transient_parent = Some(parent);
    surface.window_geometry = Some(Rect::new(12, 8, 396, 156));
    surface.min_size = (Some(320), Some(120));
    surface.max_size = (Some(1600), None);
    scene.set_visibility(dialog, Visibility::Minimized);

    let window = scene.window_state(dialog).expect("toplevel window state");
    assert_eq!(window.surface, dialog);
    assert_eq!(
        window.kind,
        WindowKind::Toplevel {
            parent: Some(parent)
        }
    );
    assert_eq!(window.title, "Confirm");
    assert_eq!(window.logical_size, Some((420, 180)));
    assert_eq!(window.geometry, Some(Rect::new(12, 8, 396, 156)));
    assert_eq!(window.min_size, (Some(320), Some(120)));
    assert_eq!(window.max_size, (Some(1600), None));
    assert_eq!(window.visibility, Visibility::Minimized);
}

#[test]
fn native_window_visibility_follows_wayland_mapping() {
    let mut scene = scene_with_output();
    let surface = scene.create_surface();
    scene.set_role(surface, SurfaceRole::Toplevel);
    assert_eq!(
        scene.window_state(surface).unwrap().visibility,
        Visibility::Occluded,
        "an xdg role without a buffer is not mapped"
    );

    commit_surface(&mut scene, surface, Commit::attach(shm(320, 200)));
    assert_eq!(
        scene.window_state(surface).unwrap().visibility,
        Visibility::Visible
    );

    commit_surface(
        &mut scene,
        surface,
        Commit {
            buffer: BufferChange::Removed,
            ..Commit::default()
        },
    );
    assert_eq!(
        scene.window_state(surface).unwrap().visibility,
        Visibility::Occluded,
        "a null-buffer commit unmaps without destroying the xdg role"
    );
}
impl FakeClock {
    fn new(start: u64) -> FakeClock {
        FakeClock {
            now: Cell::new(start),
        }
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

/// One recorded `present()` call.
#[derive(Clone, Debug, PartialEq)]
struct PresentCall {
    output: OutputId,
    surface: SurfaceId,
    width: i32,
    height: i32,
    damage: Vec<Rect>,
    timing: PresentTiming,
}

/// A presenter that records every call and replays a scripted outcome. `Delivered` by default with a
/// monotonic serial; a scripted queue overrides per call.
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
    fn present_order(&self) -> Vec<SurfaceId> {
        self.calls().into_iter().map(|c| c.surface).collect()
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    /// Force the NEXT present to return `outcome`.
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
        timing: PresentTiming,
    ) -> PresentationFeedback {
        self.calls.lock().unwrap().push(PresentCall {
            output,
            surface: image.surface,
            width: image.width,
            height: image.height,
            damage: damage.to_vec(),
            timing,
        });
        let outcome = self.scripted.lock().unwrap().pop().unwrap_or_else(|| {
            let s = self.next_serial.get();
            self.next_serial.set(s + 1);
            PresentOutcome::Delivered {
                serial: s,
                timing: Some(timing),
            }
        });
        PresentationFeedback { outcome }
    }
}

// ---- helpers -----------------------------------------------------------------------------------

fn shm(w: i32, h: i32) -> BufferState {
    BufferState {
        tex_w: w,
        tex_h: h,
        format: Format::Argb8888,
        buffer_scale: 1,
        gpu: false,
    }
}

/// A scene with one 60 Hz output (2560×1440 @ scale 1 ⇒ logical 2560×1440).
fn scene_with_output() -> Scene {
    let mut scene = Scene::new();
    scene.add_output(Output::new(OutputId(1), "hl-0", 2560, 1440, 60_000));
    scene
}

fn compositor() -> Compositor<FakePresenter, FakeClock> {
    let mut c = Compositor::new(FakePresenter::new(), FakeClock::new(0));
    c.scene
        .add_output(Output::new(OutputId(1), "hl-0", 2560, 1440, 60_000));
    c
}

/// Map a toplevel with a committed `w`×`h` shm buffer and return its id.
fn map_toplevel(scene: &mut Scene, w: i32, h: i32) -> SurfaceId {
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    commit_surface(scene, id, Commit::attach(shm(w, h)));
    id
}

// ---- 1. commit → scene + damage ----------------------------------------------------------------

#[test]
fn surface_commit_attaches_buffer_and_marks_damage() {
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);

    // A first buffer attach changes content and marks the surface dirty.
    let changed = commit_surface(&mut scene, id, Commit::attach(shm(800, 600)));
    assert!(changed, "first attach reports a content change");
    assert!(
        scene.is_dirty(id),
        "attaching a buffer marks the surface dirty"
    );
    assert_eq!(scene.get(id).unwrap().logical_size(), Some((800, 600)));

    // Clearing dirty then a frame-callback-only commit does NOT re-dirty (nothing new to show) but
    // records the pending callback.
    scene.clear_dirty(id);
    let changed = commit_surface(&mut scene, id, Commit::frame_callback_only());
    assert!(
        !changed,
        "a frame-callback-only commit reports no content change"
    );
    assert!(
        !scene.is_dirty(id),
        "a frame-callback-only commit does not dirty the surface"
    );
    assert_eq!(scene.get(id).unwrap().pending_callbacks, 1);

    // Damage against the existing buffer re-dirties and accumulates into the damage region.
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(10, 20, 30, 40)),
    );
    assert!(changed, "damage against a live buffer is a content change");
    assert!(scene.is_dirty(id));
    assert_eq!(
        scene.get(id).unwrap().damage.bounding_box(),
        Some(Rect::new(10, 20, 30, 40))
    );

    // Detach clears the buffer.
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Removed,
            ..Commit::default()
        },
    );
    assert!(changed, "detaching a live buffer is a change");
    assert!(scene.get(id).unwrap().buffer.is_none());
}

// ---- 2. compose walks the window + popup tree --------------------------------------------------

#[test]
fn compose_frame_walks_subsurfaces_and_popups_in_z_order() {
    let mut scene = scene_with_output();

    // Toplevel with two subsurfaces (bottom then top) and a popup (a menu) anchored on it.
    let top = map_toplevel(&mut scene, 1000, 700);
    let sub_bottom = scene.create_surface();
    scene.set_role(
        sub_bottom,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 10,
            y: 20,
            sync: false,
        }),
    );
    commit_surface(&mut scene, sub_bottom, Commit::attach(shm(100, 100)));
    let sub_top = scene.create_surface();
    scene.set_role(
        sub_top,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 30,
            y: 40,
            sync: false,
        }),
    );
    commit_surface(&mut scene, sub_top, Commit::attach(shm(100, 100)));

    // A popup anchored on the toplevel with resolved geometry origin (200, 150).
    let popup = scene.create_surface();
    scene.set_role(
        popup,
        SurfaceRole::Popup(hl_compositor::scene::model::PopupState {
            parent: top,
            positioner: menu_positioner(),
            geometry: Rect::new(200, 150, 180, 240),
            grabbed: true,
        }),
    );
    commit_surface(&mut scene, popup, Commit::attach(shm(180, 240)));

    let frame = scene.compose_frame(top).expect("root has a buffer");

    // Composite order (bottom → top): root, its subsurfaces (registration order = z-order), then the
    // popup on top.
    assert_eq!(frame.present_order(), vec![top, sub_bottom, sub_top, popup]);

    // Offsets: subsurfaces at their set_position; popup at its geometry origin relative to the root.
    let by_sid = |sid: SurfaceId| frame.items.iter().find(|i| i.image.surface == sid).unwrap();
    assert_eq!((by_sid(sub_bottom).x, by_sid(sub_bottom).y), (10, 20));
    assert_eq!((by_sid(sub_top).x, by_sid(sub_top).y), (30, 40));
    assert_eq!((by_sid(popup).x, by_sid(popup).y), (200, 150));

    // The popup layer carries its native placement (direct parent + geometry origin).
    let placement = by_sid(popup).image.popup.expect("popup carries placement");
    assert_eq!(
        (placement.parent, placement.x, placement.y),
        (top, 200, 150)
    );

    // Every layer is dirty (freshly attached), so the whole tree damages; each layer's damage is its
    // whole rect translated to root space.
    assert!(
        frame.damage().contains(&Rect::new(0, 0, 1000, 700)),
        "root full-surface damage"
    );
    assert!(
        frame.damage().contains(&Rect::new(200, 150, 180, 240)),
        "popup damage in root space"
    );
}

#[test]
fn compositor_presents_the_tree_layers_in_order_through_the_presenter() {
    // The FakePresenter records one present() per composed layer, in composite order — the exact seam a
    // real adapter drives. Assert against its recorded calls (not just the composed Frame).
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 1000, 700);
    let sub = c.scene.create_surface();
    c.scene.set_role(
        sub,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 30,
            y: 40,
            sync: false,
        }),
    );
    commit_surface(&mut c.scene, sub, Commit::attach(shm(100, 100)));
    let popup = c.scene.create_surface();
    c.scene
        .set_role(popup, popup_role(top, Rect::new(200, 150, 180, 240)));
    commit_surface(&mut c.scene, popup, Commit::attach(shm(180, 240)));

    let out = c.present_root(top);
    assert_eq!(out.presented, vec![top, sub, popup]);
    assert_eq!(
        c.presenter().present_order(),
        vec![top, sub, popup],
        "presenter recorded the z-order"
    );
    // The base layer's delivery timing carries the 60 Hz refresh interval.
    let base = &c.presenter().calls()[0];
    assert_eq!(base.output, OutputId(1));
    assert_eq!((base.width, base.height), (1000, 700));
    assert!(base.timing.vsync && base.timing.refresh_ns > 0);
}

#[test]
fn nested_popup_offsets_accumulate_up_the_chain() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 1000, 700);

    // Menu on the toplevel at (100, 50); submenu on the menu at (60, 30).
    let menu = scene.create_surface();
    scene.set_role(menu, popup_role(top, Rect::new(100, 50, 200, 300)));
    commit_surface(&mut scene, menu, Commit::attach(shm(200, 300)));
    let submenu = scene.create_surface();
    scene.set_role(submenu, popup_role(menu, Rect::new(60, 30, 150, 250)));
    commit_surface(&mut scene, submenu, Commit::attach(shm(150, 250)));

    let frame = scene.compose_frame(top).unwrap();
    // Parent-before-child order; submenu offset = menu origin + submenu origin.
    assert_eq!(frame.present_order(), vec![top, menu, submenu]);
    let submenu_item = frame
        .items
        .iter()
        .find(|i| i.image.surface == submenu)
        .unwrap();
    assert_eq!((submenu_item.x, submenu_item.y), (160, 80));
}

// ---- 3. popup placement math -------------------------------------------------------------------

#[test]
fn popup_placement_anchors_and_applies_gravity() {
    // Anchor at the bottom-left of a 100×20 widget at (40, 30); gravity bottom-right ⇒ the popup's
    // top-left sits at the anchor point (40, 50).
    let p = Positioner {
        anchor_rect: Rect::new(40, 30, 100, 20),
        size: (150, 200),
        anchor: Anchor::BottomLeft,
        gravity: Gravity::BottomRight,
        constraint_adjustment: ConstraintAdjustment::NONE,
        offset: (0, 0),
    };
    let geo = p.place(2000, 2000);
    assert_eq!(geo, Rect::new(40, 50, 150, 200));

    // The extra offset shifts the result.
    let p2 = Positioner {
        offset: (5, -7),
        ..p
    };
    assert_eq!(p2.place(2000, 2000), Rect::new(45, 43, 150, 200));
}

#[test]
fn popup_placement_flips_then_slides_to_stay_on_screen() {
    // A menu anchored at the right edge with gravity right would overflow; flip_y/flip_x mirror it.
    // Anchor bottom of a widget near the bottom edge; gravity bottom would run off the bottom, so
    // flip_y flips to gravity top (popup grows upward and fits).
    let p = Positioner {
        anchor_rect: Rect::new(10, 380, 40, 20), // widget bottom at y=400 in a 400-tall area
        size: (100, 150),
        anchor: Anchor::Bottom,
        gravity: Gravity::Bottom, // unflipped: top-left y = 400 → bottom = 550 > 400 (overflow)
        constraint_adjustment: ConstraintAdjustment {
            flip_y: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    // Flipped to gravity top about the flipped anchor (top, y=380): top-left y = 380 - 150 = 230.
    assert_eq!(geo.y, 230, "flip_y mirrors the popup back on-screen");
    assert!(geo.bottom() <= 400, "flipped popup fits within the output");

    // With only slide_x, an x-overflow is slid in from the right edge instead of flipped.
    let slide = Positioner {
        anchor_rect: Rect::new(460, 10, 20, 20),
        size: (100, 50),
        anchor: Anchor::Right,
        gravity: Gravity::Right, // top-left x = 480 → right = 580 > 500
        constraint_adjustment: ConstraintAdjustment {
            slide_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = slide.place(500, 400);
    assert_eq!(
        geo.right(),
        500,
        "slide_x pushes the popup flush to the right edge"
    );
    assert_eq!(geo.w, 100, "slide keeps the popup size");
}

#[test]
fn popup_placement_resizes_when_it_cannot_fit() {
    // A popup wider than the whole target, allowed only to resize, is clamped to the target width.
    let p = Positioner {
        anchor_rect: Rect::new(0, 0, 10, 10),
        size: (900, 100),
        anchor: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment {
            resize_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    assert!(
        geo.right() <= 500 && geo.x >= 0,
        "resized popup fits the target"
    );
    assert!(geo.w < 900, "resize clamps the width");
}

// ---- 4. frame scheduling / pacing across scripted clock times ----------------------------------

#[test]
fn vsync_throttle_coalesces_commits_within_one_refresh_interval() {
    // Pure schedule decision: 60 Hz ⇒ ~16.67 ms interval.
    let refresh = Output::new(OutputId(1), "o", 2560, 1440, 60_000).refresh_nanos();
    assert!(
        schedule::should_present(0, None, refresh),
        "first frame always due"
    );
    assert!(
        !schedule::should_present(refresh / 2, Some(0), refresh),
        "half an interval later: not due"
    );
    assert!(
        schedule::should_present(refresh, Some(0), refresh),
        "a full interval later: due"
    );
    assert!(
        schedule::should_present(1_000, Some(0), 0),
        "unknown refresh: always due"
    );
}

#[test]
fn compositor_paces_presents_across_scripted_clock_times() {
    let mut c = compositor();
    let refresh = c.scene.primary_output().unwrap().refresh_nanos();
    let top = map_toplevel(&mut c.scene, 800, 600);

    // t=0: first commit presents (delivered).
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(c.presenter().count(), 1);
    assert_eq!(out.serial, Some(1));

    // A new commit half an interval later is throttled — no present, frame retained.
    c.clock().set(refresh / 2);
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 10, 10)),
    );
    let out = c.present_root(top);
    assert!(
        out.throttled,
        "a commit within one refresh interval is throttled"
    );
    assert_eq!(c.presenter().count(), 1, "no new present while throttled");
    assert!(
        c.scene.is_dirty(top),
        "the throttled frame stays dirty for the next tick"
    );

    // A full interval later the retained frame presents.
    c.clock().set(refresh);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(
        c.presenter().count(),
        2,
        "the coalesced frame presents once the interval elapses"
    );
}

#[test]
fn clean_tree_skips_present_but_still_fires_frame_callbacks() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 800, 600);
    c.present_root(top); // delivers, clears dirty
    assert_eq!(c.presenter().count(), 1);

    // A frame-callback-only commit on a clean tree: no present, but the callback fires (no stall).
    let out = c.commit(top, Commit::frame_callback_only()).frame.unwrap();
    assert_eq!(out.pacing, FramePacing::Skipped);
    assert_eq!(c.presenter().count(), 1, "a clean tree does not re-present");
    assert_eq!(
        out.callbacks_fired, 1,
        "the frame-callback-only commit's callback fires"
    );
}

#[test]
fn failed_present_retains_callbacks_until_a_later_delivery() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 800, 600);

    // Script a retryable failure for the first present; its frame callback is retained, not fired.
    c.presenter_mut().script(PresentOutcome::RetryableFailure);
    let out = c
        .commit(top, Commit::attach(shm(800, 600)).into_frame_callback())
        .frame
        .unwrap();
    assert_eq!(out.pacing, FramePacing::RetryableFailure);
    assert_eq!(
        out.callbacks_fired, 0,
        "a failed present fires no callbacks"
    );
    assert_eq!(
        c.retained_callbacks(top),
        1,
        "the callback is retained for retry"
    );
    assert!(
        c.scene.is_dirty(top),
        "a failed present keeps the tree dirty"
    );

    // The next present delivers: the retained callback fires now.
    c.clock().set(1_000_000_000);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(
        out.callbacks_fired, 1,
        "the retained callback fires on the next delivery"
    );
    assert_eq!(c.retained_callbacks(top), 0);
}

#[test]
fn invisible_root_does_not_present() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 800, 600);
    c.scene.set_visibility(top, Visibility::Minimized);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::RetryableFailure);
    assert_eq!(
        c.presenter().count(),
        0,
        "a minimized root never reaches the presenter"
    );
}

#[test]
fn pacing_policy_matches_the_ported_state_machine() {
    assert!(FramePacing::Presented.policy().complete_callbacks);
    assert!(FramePacing::Presented.policy().present_feedback);
    assert!(FramePacing::Skipped.policy().complete_callbacks);
    assert!(!FramePacing::Skipped.policy().present_feedback);
    assert!(FramePacing::RetryableFailure.policy().retain);
    assert!(FramePacing::TerminalFailure.policy().terminal_cleanup);
    // Outcome → pacing mapping: Offscreen is a retryable failure, not a delivery.
    assert_eq!(
        FramePacing::from(PresentOutcome::Offscreen),
        FramePacing::RetryableFailure
    );
    assert_eq!(
        FramePacing::from(PresentOutcome::Delivered {
            serial: 9,
            timing: None
        }),
        FramePacing::Presented
    );
}

// ---- 5. compose skip via conservative occlusion ------------------------------------------------

#[test]
fn fully_occluded_surface_does_not_force_a_present() {
    // The ported `tree_dirty` skips a present only when a dirty surface's WHOLE rectangle is provably
    // covered by an opaque surface composited above it (not merely a damage sub-region).
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 1000, 700);

    // A small background subsurface, then a larger OPAQUE foreground fully covering it (registered
    // after ⇒ composited above).
    let bg = scene.create_surface();
    scene.set_role(
        bg,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 0,
            y: 0,
            sync: false,
        }),
    );
    commit_surface(&mut scene, bg, Commit::attach(shm(100, 100)));
    let fg = scene.create_surface();
    scene.set_role(
        fg,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 0,
            y: 0,
            sync: false,
        }),
    );
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(BufferState {
                format: Format::Xrgb8888,
                ..shm(200, 200)
            }),
            opaque_region: Some(Some(Rect::new(0, 0, 200, 200))),
            ..Commit::default()
        },
    );

    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);
    assert!(
        !c.scene.any_dirty(),
        "the initial present clears the whole tree"
    );

    // Damage ONLY the fully-covered background subsurface: its whole rect is under the opaque fg, so
    // the change is not visible and no present is forced.
    c.apply_commit(
        bg,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(10, 10, 20, 20)),
    );
    assert!(
        !c.scene.is_tree_dirty(top),
        "a surface fully behind an opaque cover is not visible"
    );

    // Damaging the root (whose full 1000×700 rect the 200×200 cover cannot occlude) IS visible.
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(500, 500, 20, 20)),
    );
    assert!(
        c.scene.is_tree_dirty(top),
        "an un-occluded surface forces a present"
    );
}

// ---- 6. focus on window raise / close ----------------------------------------------------------

#[test]
fn focus_follows_window_map_raise_and_close() {
    let mut scene = scene_with_output();
    let a = map_toplevel(&mut scene, 800, 600);
    let b = map_toplevel(&mut scene, 800, 600);

    // Map/focus A, then B.
    let ch = scene.focus(a);
    assert_eq!((ch.previous, ch.current), (None, Some(a)));
    assert!(scene.seat().has_keyboard_focus(a));

    let ch = scene.activate(b);
    assert!(ch.changed());
    assert_eq!((ch.previous, ch.current), (Some(a), Some(b)));
    assert!(scene.seat().has_keyboard_focus(b));

    // Closing B (the focused window) drops focus to nothing (no auto-refocus).
    let ch = scene.on_window_gone(b);
    assert_eq!((ch.previous, ch.current), (Some(b), None));
    assert_eq!(scene.seat().keyboard_focus, None);

    // Closing an UNFOCUSED window does not touch focus.
    scene.focus(a);
    let ch = scene.on_window_gone(b);
    assert!(!ch.changed());
    assert_eq!(scene.seat().keyboard_focus, Some(a));
}

#[test]
fn pointer_hit_test_returns_topmost_input_surface() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 1000, 700);
    let child = scene.create_surface();
    scene.set_role(
        child,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 100,
            y: 100,
            sync: false,
        }),
    );
    commit_surface(&mut scene, child, Commit::attach(shm(200, 200)));

    // Inside the child (topmost) region → hits the child.
    let hit = focus::update_pointer(&mut scene, top, 150.0, 150.0);
    assert_eq!(hit, Some(child));
    assert_eq!(scene.seat().pointer_focus, Some(child));

    // Outside the child but inside the root → hits the root.
    let hit = focus::update_pointer(&mut scene, top, 10.0, 10.0);
    assert_eq!(hit, Some(top));
}

// ---- small test helpers ------------------------------------------------------------------------

fn popup_role(parent: SurfaceId, geometry: Rect) -> SurfaceRole {
    SurfaceRole::Popup(hl_compositor::scene::model::PopupState {
        parent,
        positioner: menu_positioner(),
        geometry,
        grabbed: false,
    })
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

/// Chain a frame-callback request onto a buffer-attaching commit (used by the retain test).
trait CommitExt {
    fn into_frame_callback(self) -> Commit;
}
impl CommitExt for Commit {
    fn into_frame_callback(mut self) -> Commit {
        self.frame_callback = true;
        self
    }
}
