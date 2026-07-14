//! The platform-neutral compositor policy — the "brain".
//!
//! `scene` owns the neutral [`model`] (the window/subsurface/popup graph over outputs, with a seat and
//! damage), reaches the world only through two [`port`] traits ([`port::Presenter`], [`port::Clock`]),
//! and runs its rules as the [`service`] use-cases (commit / popup / compose / schedule / focus). It
//! contains NO Smithay, NO GPU, NO Cocoa/DRM: a future `adapter/smithay` will translate Wayland
//! callbacks into these service calls, and `adapter/{cocoa,drm}` will implement [`port::Presenter`].
//!
//! [`Compositor`] is the thin wiring object that binds a [`Scene`], a `Presenter`, and a `Clock`
//! together and drives the commit → compose → present → pace path — the neutral analogue of the loop
//! `HlState::on_commit` / `present_render_root` runs, testable end to end with a `FakeClock` +
//! `FakePresenter`.

pub mod model;
pub mod port;
pub mod service;

use std::collections::HashMap;

use model::{Scene, SurfaceId, SurfaceRole};
use port::{Clock, PresentOutcome, Presenter};
use service::{
    commit_surface, compose_frame, is_tree_dirty, schedule, Commit, FramePacing,
};

/// Terminal bound on callbacks retained across failed presents (mirrors `MAX_RETAINED_CALLBACKS`): a
/// permanently-dead presenter must not grow a surface's retained-callback queue without limit.
const MAX_RETAINED_CALLBACKS: u32 = 16;

/// The outcome of driving one present root through compose → present → pace.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameOutcome {
    /// How the tree paced this cycle.
    pub pacing: FramePacing,
    /// The surfaces `present()` was called for, in composite order (bottom → top).
    pub presented: Vec<SurfaceId>,
    /// Total `wl_surface.frame` callbacks fired across the tree this cycle.
    pub callbacks_fired: u32,
    /// The delivery serial, when the base frame was `Delivered`.
    pub serial: Option<u64>,
    /// The frame was withheld by the vsync throttle (not yet due) — no present attempted, no callbacks
    /// fired. The retained frame still stands and will present on a later tick.
    pub throttled: bool,
}

/// The outcome of a `commit`: whether the surface's content changed, and the present it triggered (if
/// any — a cursor or synchronized-subsurface commit does not present on its own).
#[derive(Clone, Debug, PartialEq)]
pub struct CommitOutcome {
    pub changed: bool,
    pub frame: Option<FrameOutcome>,
}

/// Binds the neutral [`Scene`] to a concrete [`Presenter`] + [`Clock`] and runs the compositor policy.
pub struct Compositor<P: Presenter, C: Clock> {
    /// The neutral scene graph — public so an adapter/test can inspect and drive it directly.
    pub scene: Scene,
    presenter: P,
    clock: C,
    /// Per-root monotonic time of the last delivered present (drives the vsync throttle).
    last_present_ns: HashMap<SurfaceId, u64>,
    /// Per-surface count of frame callbacks retained across failed presents (bounded).
    retained_callbacks: HashMap<SurfaceId, u32>,
}

impl<P: Presenter, C: Clock> Compositor<P, C> {
    pub fn new(presenter: P, clock: C) -> Compositor<P, C> {
        Compositor {
            scene: Scene::new(),
            presenter,
            clock,
            last_present_ns: HashMap::new(),
            retained_callbacks: HashMap::new(),
        }
    }

    /// Construct over an existing scene (e.g. one an adapter pre-populated with outputs).
    pub fn with_scene(scene: Scene, presenter: P, clock: C) -> Compositor<P, C> {
        Compositor { scene, presenter, clock, last_present_ns: HashMap::new(), retained_callbacks: HashMap::new() }
    }

    pub fn presenter(&self) -> &P {
        &self.presenter
    }
    pub fn presenter_mut(&mut self) -> &mut P {
        &mut self.presenter
    }
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Callbacks retained for `sid` across failed presents (test/diagnostic).
    pub fn retained_callbacks(&self, sid: SurfaceId) -> u32 {
        self.retained_callbacks.get(&sid).copied().unwrap_or(0)
    }

    /// The host-monotonic time at which `root` next becomes due to present — its last delivered present
    /// plus the output's refresh interval — or `None` if it has never presented or has no output.
    ///
    /// This is the exact boundary [`present_root`] tests with [`schedule::should_present`]: a commit that
    /// arrives before it returns `throttled: true`. An adapter uses this to arm a repaint at that instant,
    /// so a throttled frame still ships ~one refresh later even if the client then goes idle (the retained
    /// frame otherwise never re-drives — nothing else calls `present_root`). Saturating so a pathological
    /// `last + refresh` overflow clamps instead of wrapping to an always-due time.
    pub fn next_present_due_ns(&self, root: SurfaceId) -> Option<u64> {
        let last = *self.last_present_ns.get(&root)?;
        let refresh = self.scene.selected_output(root)?.refresh_nanos();
        Some(last.saturating_add(refresh))
    }

    /// Apply a commit to a surface WITHOUT presenting (pure state update); returns whether content
    /// changed. Use [`Self::commit`] to also run the present decision.
    pub fn apply_commit(&mut self, sid: SurfaceId, commit: Commit) -> bool {
        commit_surface(&mut self.scene, sid, commit)
    }

    /// The full commit → present path (the neutral `on_commit`): apply the commit, then — unless this is
    /// a cursor or a synchronized subsurface (which present as part of their parent) — present the
    /// surface's window root.
    pub fn commit(&mut self, sid: SurfaceId, commit: Commit) -> CommitOutcome {
        let changed = self.apply_commit(sid, commit);
        let role = self.scene.get(sid).map(|s| s.role.clone());
        match role {
            // A cursor image is turned into a host cursor, never presented as a window.
            Some(SurfaceRole::Cursor) => CommitOutcome { changed, frame: None },
            // A synchronized subsurface presents atomically with its parent; do not present now.
            Some(SurfaceRole::Subsurface(s)) if s.sync => CommitOutcome { changed, frame: None },
            _ => {
                let frame = self
                    .scene
                    .window_root(sid)
                    .map(|root| self.present_root(root));
                CommitOutcome { changed, frame }
            }
        }
    }

    /// Compose + present the tree rooted at `root`, then advance frame pacing. The neutral analogue of
    /// `present_render_root`: an invisible root or a clean tree short-circuits; a due, dirty, visible
    /// tree is composed into ordered layers, each presented through the port, and paced on the base
    /// layer's outcome.
    pub fn present_root(&mut self, root: SurfaceId) -> FrameOutcome {
        // A root with no output is unpresentable.
        let Some(output) = self.scene.selected_output(root).cloned() else {
            let fired = self.pace_tree(root, FramePacing::TerminalFailure);
            return FrameOutcome { pacing: FramePacing::TerminalFailure, presented: Vec::new(), callbacks_fired: fired, serial: None, throttled: false };
        };

        // Hidden/minimized/occluded root: the frame did not reach the screen — retain pacing.
        if !self.scene.visibility(root).allows_present() {
            let fired = self.pace_tree(root, FramePacing::RetryableFailure);
            return FrameOutcome { pacing: FramePacing::RetryableFailure, presented: Vec::new(), callbacks_fired: fired, serial: None, throttled: false };
        }

        // Nothing visible changed: the previous frame still stands. Fire callbacks, discard feedback.
        if !is_tree_dirty(&self.scene, root) {
            let fired = self.pace_tree(root, FramePacing::Skipped);
            return FrameOutcome { pacing: FramePacing::Skipped, presented: Vec::new(), callbacks_fired: fired, serial: None, throttled: false };
        }

        // Vsync throttle: coalesce a burst of commits within one refresh interval into one present.
        let now = self.clock.now_nanos();
        let refresh = output.refresh_nanos();
        let last = self.last_present_ns.get(&root).copied();
        if !schedule::should_present(now, last, refresh) {
            return FrameOutcome { pacing: FramePacing::Skipped, presented: Vec::new(), callbacks_fired: 0, serial: None, throttled: true };
        }

        // Compose the ordered layers, then present each through the port (scene borrow released first).
        let Some(frame) = compose_frame(&self.scene, root) else {
            let fired = self.pace_tree(root, FramePacing::TerminalFailure);
            return FrameOutcome { pacing: FramePacing::TerminalFailure, presented: Vec::new(), callbacks_fired: fired, serial: None, throttled: false };
        };
        let timing = schedule::fallback_timing(now, refresh);

        let output_id = output.id;
        let mut presented = Vec::new();
        let mut base_outcome: Option<PresentOutcome> = None;
        for item in &frame.items {
            let feedback = self.presenter.present(output_id, &item.image, &item.damage, timing);
            presented.push(item.image.surface);
            base_outcome.get_or_insert(feedback.outcome);
        }

        let pacing = base_outcome.map(schedule::from_outcome).unwrap_or(FramePacing::TerminalFailure);
        let serial = match base_outcome {
            Some(PresentOutcome::Delivered { serial, .. }) => Some(serial),
            _ => None,
        };

        if pacing == FramePacing::Presented {
            self.clear_tree_dirty(root);
            self.last_present_ns.insert(root, now);
        }
        let fired = self.pace_tree(root, pacing);
        FrameOutcome { pacing, presented, callbacks_fired: fired, serial, throttled: false }
    }

    /// Every surface in `root`'s presented tree (root + subsurface descendants + popups + their
    /// descendants) — the breadth `pace_tree` / `clear_tree_dirty` operate over.
    fn present_tree_surfaces(&self, root: SurfaceId) -> Vec<SurfaceId> {
        let mut surfaces = Vec::new();
        self.scene.collect_tree_surfaces(root, &mut surfaces);
        for (popup, _, _) in self.scene.collect_popups_for_root(root) {
            self.scene.collect_tree_surfaces(popup, &mut surfaces);
        }
        surfaces
    }

    /// Advance frame pacing for every surface in the tree: fire, retain, or drop each surface's pending
    /// `wl_surface.frame` callbacks per the pacing policy. Returns the total fired. Neutral port of
    /// `pace_tree` / `pace_surface` (callbacks modelled as a count; feedback objects elided).
    fn pace_tree(&mut self, root: SurfaceId, pacing: FramePacing) -> u32 {
        let policy = pacing.policy();
        let surfaces = self.present_tree_surfaces(root);
        let mut fired = 0u32;
        for sid in surfaces {
            let pending = match self.scene.get_mut(sid) {
                Some(s) => std::mem::take(&mut s.pending_callbacks),
                None => 0,
            };
            if policy.complete_callbacks {
                fired += self.retained_callbacks.remove(&sid).unwrap_or(0) + pending;
            } else if policy.retain {
                let q = self.retained_callbacks.entry(sid).or_insert(0);
                *q = (*q + pending).min(MAX_RETAINED_CALLBACKS);
            } else {
                // Terminal: drop retained + pending without firing.
                self.retained_callbacks.remove(&sid);
            }
        }
        fired
    }

    /// Clear the dirty flag + accumulated damage for every surface in `root`'s presented tree (after a
    /// successful present — the whole tree is now on screen). Port of `clear_tree_dirty`.
    fn clear_tree_dirty(&mut self, root: SurfaceId) {
        for sid in self.present_tree_surfaces(root) {
            self.scene.clear_dirty(sid);
            if let Some(s) = self.scene.get_mut(sid) {
                s.damage.clear();
            }
        }
    }
}
