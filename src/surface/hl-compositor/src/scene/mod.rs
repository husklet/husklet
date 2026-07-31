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

use std::collections::{HashMap, HashSet};

use hl_log::{hl_count, hl_debug, hl_span, tag};

use model::{Scene, SurfaceId, SurfaceRole};
use port::{
    Clock, CompletionOutcome, PresentOutcome, PresentTiming, PresentationCompletion,
    PresentationId, Presenter,
};
use service::{commit_surface, schedule, Commit, FramePacing};

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
    /// Asynchronous host-display submission still awaiting terminal presentation evidence.
    pub submissions: Vec<PresentationId>,
    /// The frame was withheld by the vsync throttle (not yet due) — no present attempted, no callbacks
    /// fired. The retained frame still stands and will present on a later tick.
    pub throttled: bool,
}

struct PendingFrame {
    root: SurfaceId,
    submissions: HashSet<PresentationId>,
    callbacks: HashMap<SurfaceId, u32>,
    deliver: bool,
    failed: bool,
    /// A submission in this frame completed retryably (the frame did not reach the screen, but may next
    /// time). Distinct from `failed`: it retains the frame's callbacks instead of dropping them.
    retryable: bool,
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
    pending_frames: HashMap<SurfaceId, PendingFrame>,
    pending_submissions: HashMap<PresentationId, SurfaceId>,
    pending_roots: HashMap<SurfaceId, PresentationId>,
    /// How many times each root has dropped a frame before the presenter, per reason (see
    /// [`Self::announce_stall`]). Counted, not merely latched — the count is what says whether a root is
    /// recovering or stuck.
    announced_stalls: crate::diagnostic::Tally<(SurfaceId, &'static str)>,
}

impl<P: Presenter, C: Clock> Compositor<P, C> {
    pub fn new(presenter: P, clock: C) -> Compositor<P, C> {
        Compositor {
            scene: Scene::new(),
            presenter,
            clock,
            last_present_ns: HashMap::new(),
            retained_callbacks: HashMap::new(),
            pending_frames: HashMap::new(),
            pending_submissions: HashMap::new(),
            pending_roots: HashMap::new(),
            announced_stalls: crate::diagnostic::Tally::new(),
        }
    }

    /// Construct over an existing scene (e.g. one an adapter pre-populated with outputs).
    pub fn with_scene(scene: Scene, presenter: P, clock: C) -> Compositor<P, C> {
        Compositor {
            scene,
            presenter,
            clock,
            last_present_ns: HashMap::new(),
            retained_callbacks: HashMap::new(),
            pending_frames: HashMap::new(),
            pending_submissions: HashMap::new(),
            pending_roots: HashMap::new(),
            announced_stalls: crate::diagnostic::Tally::new(),
        }
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

    /// Cancel an in-flight atomic frame and detach every completion route for it. Native resources may
    /// finish asynchronously in the presenter, but late completion IDs can no longer affect this root.
    pub fn cancel_root(&mut self, root: SurfaceId) {
        if let Some(frame) = self.pending_frames.remove(&root) {
            for submission in frame.submissions {
                self.pending_submissions.remove(&submission);
            }
        }
        self.pending_roots.remove(&root);
        self.presenter.cancel(root);
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
        self.complete_commit(sid, changed)
    }

    /// Finish presentation after an adapter has applied a commit and reconciled external window state.
    /// Keeping this phase explicit prevents a backend from learning geometry/ownership one frame late.
    pub fn complete_commit(&mut self, sid: SurfaceId, changed: bool) -> CommitOutcome {
        let role = self.scene.get(sid).map(|s| s.role.clone());
        match role {
            // A cursor image is turned into a host cursor, never presented as a window.
            Some(SurfaceRole::Cursor) => CommitOutcome {
                changed,
                frame: None,
            },
            // A synchronized subsurface presents atomically with its parent; do not present now.
            Some(SurfaceRole::Subsurface(s)) if s.sync => CommitOutcome {
                changed,
                frame: None,
            },
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
        let _span = hl_span!(tag::PRESENT, "present");
        if let Some(&submission) = self.pending_roots.get(&root) {
            return FrameOutcome {
                pacing: FramePacing::Pending,
                presented: Vec::new(),
                callbacks_fired: 0,
                serial: None,
                submissions: vec![submission],
                throttled: false,
            };
        }
        // A root with no output is unpresentable.
        let Some(output) = self.scene.selected_output(root).cloned() else {
            self.announce_stall(root, "no_selected_output");
            let fired = self.pace_tree(root, FramePacing::TerminalFailure);
            return FrameOutcome {
                pacing: FramePacing::TerminalFailure,
                presented: Vec::new(),
                callbacks_fired: fired,
                serial: None,
                submissions: Vec::new(),
                throttled: false,
            };
        };

        // Hidden/minimized/occluded root: the frame did not reach the screen — retain pacing.
        if !self.scene.visibility(root).allows_present() {
            let fired = self.pace_tree(root, FramePacing::RetryableFailure);
            return FrameOutcome {
                pacing: FramePacing::RetryableFailure,
                presented: Vec::new(),
                callbacks_fired: fired,
                serial: None,
                submissions: Vec::new(),
                throttled: false,
            };
        }

        // Nothing visible changed: the previous frame still stands. Fire callbacks, discard feedback.
        if !self.scene.is_tree_dirty(root) {
            hl_count!(tag::PRESENT, "skipped");
            let fired = self.pace_tree(root, FramePacing::Skipped);
            return FrameOutcome {
                pacing: FramePacing::Skipped,
                presented: Vec::new(),
                callbacks_fired: fired,
                serial: None,
                submissions: Vec::new(),
                throttled: false,
            };
        }

        // Vsync throttle: coalesce a burst of commits within one refresh interval into one present.
        let now = self.clock.now_nanos();
        let refresh = output.refresh_nanos();
        let last = self.last_present_ns.get(&root).copied();
        if !schedule::should_present(now, last, refresh) {
            hl_count!(tag::PRESENT, "throttled");
            return FrameOutcome {
                pacing: FramePacing::Skipped,
                presented: Vec::new(),
                callbacks_fired: 0,
                serial: None,
                submissions: Vec::new(),
                throttled: true,
            };
        }

        // Compose the ordered layers, then present each through the port (scene borrow released first).
        let timing = PresentTiming::fallback(now, refresh);
        let Some(frames) = self.scene.compose_present_frames(root, output.id, timing) else {
            self.announce_stall(root, "compose_produced_no_frame");
            let fired = self.pace_tree(root, FramePacing::TerminalFailure);
            return FrameOutcome {
                pacing: FramePacing::TerminalFailure,
                presented: Vec::new(),
                callbacks_fired: fired,
                serial: None,
                submissions: Vec::new(),
                throttled: false,
            };
        };
        let mut presented = Vec::new();
        let mut outcomes = Vec::with_capacity(frames.len());
        for frame in &frames {
            let feedback = self.presenter.present_frame(frame);
            presented.push(frame.role);
            outcomes.push(feedback.outcome);
        }
        let submissions = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                PresentOutcome::Pending { id } => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        let unique_submissions = submissions.iter().copied().collect::<HashSet<_>>();
        let submission_conflict = unique_submissions
            .iter()
            .any(|id| self.pending_submissions.contains_key(id));
        if unique_submissions.len() != submissions.len() || submission_conflict {
            for frame in &frames {
                self.presenter.cancel(frame.role);
            }
            let fired = self.pace_tree(root, FramePacing::TerminalFailure);
            return FrameOutcome {
                pacing: FramePacing::TerminalFailure,
                presented,
                callbacks_fired: fired,
                serial: None,
                submissions: Vec::new(),
                throttled: false,
            };
        }
        let failed = outcomes.contains(&PresentOutcome::TerminalFailure);
        let retry = outcomes.iter().any(|outcome| {
            matches!(
                outcome,
                PresentOutcome::Offscreen | PresentOutcome::RetryableFailure
            )
        });
        let serial = outcomes.iter().find_map(|outcome| match outcome {
            PresentOutcome::Delivered { serial, .. } => Some(*serial),
            _ => None,
        });
        let pacing = if submissions.is_empty() {
            if failed {
                FramePacing::TerminalFailure
            } else if retry {
                FramePacing::RetryableFailure
            } else if serial.is_some() {
                FramePacing::Presented
            } else {
                FramePacing::TerminalFailure
            }
        } else {
            FramePacing::Pending
        };
        if !submissions.is_empty() {
            let callbacks = self.take_submission_callbacks(root);
            let deliver = !failed && !retry;
            if deliver {
                self.clear_tree_dirty(root);
            }
            let ids = unique_submissions;
            for id in &ids {
                self.pending_submissions.insert(*id, root);
            }
            self.pending_frames.insert(
                root,
                PendingFrame {
                    root,
                    submissions: ids,
                    callbacks,
                    deliver,
                    failed,
                    retryable: retry,
                },
            );
            self.pending_roots.insert(root, submissions[0]);
        } else if pacing == FramePacing::Presented {
            hl_count!(tag::PRESENT, "presented");
            // Latency trace: stamp the host-monotonic time this frame actually reached the presenter (the
            // END of the input→present cycle). Pairs with the adapter's `input_dispatch … t_us=` line so a
            // trace can subtract them for the real input→present latency. Terse key=val, gated.
            hl_debug!(
                tag::PRESENT,
                "present_done root={} serial={:?} t_us={}",
                root.0,
                serial,
                now / 1_000
            );
            self.clear_tree_dirty(root);
            self.last_present_ns.insert(root, now);
        }
        let fired = if !submissions.is_empty() {
            0
        } else {
            self.pace_tree(root, pacing)
        };
        FrameOutcome {
            pacing,
            presented,
            callbacks_fired: fired,
            serial,
            submissions,
            throttled: false,
        }
    }

    /// Settle one asynchronous host presentation. Unknown or duplicate IDs are ignored.
    pub fn complete_presentation(
        &mut self,
        completion: PresentationCompletion,
    ) -> Option<(SurfaceId, FrameOutcome)> {
        let root = self.pending_submissions.remove(&completion.id)?;
        let pending = self.pending_frames.get_mut(&root)?;
        pending.submissions.remove(&completion.id);
        match completion.outcome {
            CompletionOutcome::TerminalFailure => pending.failed = true,
            CompletionOutcome::RetryableFailure => pending.retryable = true,
            CompletionOutcome::Delivered { .. } => {}
        }
        if !pending.submissions.is_empty() {
            return None;
        }
        let pending = self.pending_frames.remove(&root)?;
        self.pending_roots.remove(&root);
        let (pacing, serial, callbacks_fired) = match completion.outcome {
            CompletionOutcome::Delivered {
                serial: _,
                timing: _,
            } if pending.failed => (FramePacing::TerminalFailure, None, 0),
            CompletionOutcome::Delivered {
                serial: _,
                timing: _,
            } if pending.retryable || !pending.deliver => (FramePacing::RetryableFailure, None, 0),
            CompletionOutcome::Delivered { serial, timing: _ } => {
                // The backend timestamp may use the host's boot-time epoch while this injected clock may
                // use any fixed epoch. Pacing compares values only within the injected clock domain.
                self.last_present_ns
                    .insert(pending.root, self.clock.now_nanos());
                (
                    FramePacing::Presented,
                    Some(serial),
                    pending.callbacks.values().copied().sum(),
                )
            }
            // A retryable completion keeps the frame's callbacks alive for the next accepted present; only
            // a terminal one drops them.
            CompletionOutcome::RetryableFailure if !pending.failed => {
                (FramePacing::RetryableFailure, None, 0)
            }
            CompletionOutcome::RetryableFailure | CompletionOutcome::TerminalFailure => {
                (FramePacing::TerminalFailure, None, 0)
            }
        };
        // Retryable pacing means RETAIN — the same contract `pace_tree` honours on the synchronous path.
        // Without this the callbacks a frame carried were simply dropped when its submission came back
        // undelivered, so a client waiting on `wl_surface.frame` never heard again from a frame the host
        // fully intends to re-drive.
        if pacing == FramePacing::RetryableFailure {
            for (sid, count) in pending.callbacks {
                let queued = self.retained_callbacks.entry(sid).or_insert(0);
                *queued = (*queued + count).min(MAX_RETAINED_CALLBACKS);
            }
        }
        Some((
            root,
            FrameOutcome {
                pacing,
                presented: Vec::new(),
                callbacks_fired,
                serial,
                submissions: vec![completion.id],
                throttled: false,
            },
        ))
    }

    fn take_submission_callbacks(&mut self, root: SurfaceId) -> HashMap<SurfaceId, u32> {
        let mut callbacks = HashMap::new();
        for sid in self.present_tree_surfaces(root) {
            let current = self
                .scene
                .get_mut(sid)
                .map(|surface| std::mem::take(&mut surface.pending_callbacks))
                .unwrap_or(0);
            let retained = self.retained_callbacks.remove(&sid).unwrap_or(0);
            let count = current.saturating_add(retained);
            if count > 0 {
                callbacks.insert(sid, count);
            }
        }
        callbacks
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
    /// Name a frame the policy dropped before it ever reached the presenter, once per root per reason.
    ///
    /// These two branches were the last places a frame could vanish in complete silence: they return
    /// `TerminalFailure`, which drops the client's `wl_surface.frame` callbacks, and they do it without
    /// touching the port — so no presenter attribution can ever fire for them and a window simply stops.
    /// Counted always, logged once, so a permanently unpresentable root costs two lines rather than one
    /// per commit for the life of the process.
    fn announce_stall(&mut self, root: SurfaceId, reason: &'static str) {
        hl_count!(tag::PRESENT, "present_unpresentable");
        let Some(seen) = self.announced_stalls.record((root, reason)) else {
            return;
        };
        hl_log::hl_log!(
            tag::PRESENT,
            hl_log::Level::Error,
            "present unpresentable root={} reason={reason} count={} over_ms={} — the frame never \
             reached the presenter and this client's frame callbacks are dropped; read the rate, not \
             the count",
            root.0,
            seen.count,
            seen.since.as_millis()
        );
    }

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
