//! Adversarial / edge-case coverage for the platform-neutral compositor policy (`scene`).
//!
//! Complements `tests/scene.rs` (the happy-path proofs) with the malformed, boundary, and invariant
//! cases the mission demands: zero/1×1/huge/off-screen/overflow geometry, out-of-order commit/attach,
//! missing/unknown surfaces, HiDPI + viewport sizing, damage bounds, z-order stability, subsurface +
//! popup linkage, popup constraint math corners, focus/hit-test edges, and frame pacing across scripted
//! clock anomalies. Everything runs against a `FakeClock` + a recording `FakePresenter` — no Smithay, no
//! GPU. Assertions target real composed output (present order, layer offsets, damage rects, pacing),
//! never "it didn't panic".

#[path = "scene_adversarial/commit.rs"]
mod commit;
#[path = "scene_adversarial/composition.rs"]
mod composition;
#[path = "scene_adversarial/geometry.rs"]
mod geometry;
#[path = "scene_adversarial/input.rs"]
mod input;
#[path = "scene_adversarial/pacing.rs"]
mod pacing;
#[path = "scene_adversarial/popup.rs"]
mod popup;
#[path = "scene_adversarial/presentation.rs"]
mod presentation;

use std::cell::Cell;
use std::sync::Mutex;

use hl_compositor::scene::model::{
    Anchor, BufferState, BufferTransform, ConstraintAdjustment, DamageRegion, Format, Gravity,
    Output, OutputId, PopupState, Positioner, PresentableImage, Rect, Scene, SubsurfaceState,
    Surface, SurfaceId, SurfaceRole, Viewport, Visibility,
};
use hl_compositor::scene::port::{
    Clock, PresentOutcome, PresentTiming, PresentationFeedback, Presenter,
};
use hl_compositor::scene::service::{
    commit_surface, focus, schedule, BufferChange, Commit, FramePacing,
};
use hl_compositor::Compositor;

// ---- fakes -------------------------------------------------------------------------------------

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
            PresentOutcome::Delivered {
                serial: s,
                timing: None,
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
fn xrgb(w: i32, h: i32) -> BufferState {
    BufferState {
        tex_w: w,
        tex_h: h,
        format: Format::Xrgb8888,
        buffer_scale: 1,
        gpu: false,
    }
}

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

fn map_toplevel(scene: &mut Scene, w: i32, h: i32) -> SurfaceId {
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    commit_surface(scene, id, Commit::attach(shm(w, h)));
    id
}

fn sub(parent: SurfaceId, x: i32, y: i32) -> SurfaceRole {
    SurfaceRole::Subsurface(SubsurfaceState {
        parent,
        x,
        y,
        sync: false,
    })
}

fn popup_role(parent: SurfaceId, geometry: Rect) -> SurfaceRole {
    SurfaceRole::Popup(PopupState {
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

// =================================================================================================
// 1. Rect geometry: the primitive every damage/occlusion/hit decision rests on
// =================================================================================================
