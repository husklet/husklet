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

#[path = "scene/composition.rs"]
mod composition;
#[path = "scene/focus.rs"]
mod focus_tests;
#[path = "scene/pacing.rs"]
mod pacing;
#[path = "scene/popup.rs"]
mod popup;
#[path = "scene/window.rs"]
mod window;

// ---- fakes -------------------------------------------------------------------------------------

/// A clock that returns whatever nanos the test scripts (mutable via [`FakeClock::set`]).
struct FakeClock {
    now: Cell<u64>,
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
