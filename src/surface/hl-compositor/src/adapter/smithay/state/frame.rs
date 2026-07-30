use super::*;

pub(super) struct HostFrame {
    root: SurfaceId,
    callbacks: HashMap<SurfaceId, Vec<WlCallback>>,
    feedbacks: HashMap<SurfaceId, Vec<PresentationFeedbackCallback>>,
}

impl HlState {
    pub(super) fn cancel_host_root(&mut self, root: SurfaceId) {
        self.engine.cancel_root(root);
        self.pending_repaints.remove(&root);
        if let Some(host) = self.pending_host_frames.remove(&root) {
            for feedback in host.feedbacks.into_values().flatten() {
                feedback.discarded();
            }
        }
    }

    /// Act on the pacing outcome of a just-driven present for window root `root`: fire, retain, or drop
    /// the frame callbacks held for its tree, and arm/clear a repaint so a withheld frame still ships.
    ///
    /// Presentation feedback follows the same pacing outcome, but only real presented pixels answer
    /// `presented`; clean skipped frames retain feedback until later presentation or teardown.
    pub(super) fn settle_frame(&mut self, root: SurfaceId, frame: &FrameOutcome) {
        if frame.throttled {
            hl_debug!(
                tag::PRESENT,
                "settle root={} throttled=1 (repaint armed)",
                root.0
            );
            self.arm_repaint(root);
            return;
        }
        if frame.pacing == crate::scene::service::FramePacing::Pending {
            if frame.submissions.is_empty() {
                return;
            }
            // A later commit while A is in flight receives the same Pending snapshot from the scene.
            // Its callbacks belong to B and must remain queued for the repaint after A completes.
            if self.pending_host_frames.contains_key(&root) {
                return;
            }
            self.pending_repaints.remove(&root);
            let targets = self.tree_protocol_surfaces(root);
            let mut callbacks = HashMap::new();
            let mut feedbacks = HashMap::new();
            for surface in targets {
                if let Some(values) = self.pending_callbacks.remove(&surface) {
                    callbacks.insert(surface, values);
                }
                if let Some(values) = self.pending_presentation.remove(&surface) {
                    feedbacks.insert(surface, values);
                }
            }
            self.pending_host_frames.insert(
                root,
                HostFrame {
                    root,
                    callbacks,
                    feedbacks,
                },
            );
            return;
        }
        let policy = frame.pacing.policy();
        hl_debug!(
            tag::PRESENT,
            "settle root={} present_feedback={} complete_cb={} terminal={}",
            root.0,
            policy.present_feedback,
            policy.complete_callbacks,
            policy.terminal_cleanup
        );
        if policy.complete_callbacks {
            self.pending_repaints.remove(&root);
            self.fire_tree_callbacks(root);
        } else if policy.terminal_cleanup {
            self.pending_repaints.remove(&root);
            self.drop_tree_callbacks(root);
        } else {
            // Retryable: retain the callbacks and try again on a later tick.
            self.arm_repaint(root);
        }
        // Presentation feedback: `presented` only for a real pixel present; `discarded` on terminal cleanup.
        // A `Skipped` frame leaves it held (answered when the tree next actually presents, or discarded at
        // teardown).
        if policy.present_feedback {
            self.answer_tree_feedback(root, true);
        } else if policy.terminal_cleanup {
            self.answer_tree_feedback(root, false);
        }
    }

    pub(crate) fn complete_host_presentation(
        &mut self,
        completion: crate::scene::port::PresentationCompletion,
    ) {
        let Some((root, frame)) = self.engine.complete_presentation(completion) else {
            return;
        };
        let Some(host) = self.pending_host_frames.remove(&root) else {
            return;
        };
        match frame.pacing {
            crate::scene::service::FramePacing::Presented => {
                let timing = match completion.outcome {
                    crate::scene::port::CompletionOutcome::Delivered { timing, .. } => timing,
                    crate::scene::port::CompletionOutcome::TerminalFailure => None,
                };
                let time_ns = timing
                    .map(|timing| timing.present_ns)
                    .or_else(crate::scene::port::clock::monotonic_nanos)
                    .unwrap_or(0);
                let time_ms = (time_ns / 1_000_000) as u32;
                for callback in host.callbacks.into_values().flatten() {
                    callback.done(time_ms);
                }
                self.answer_host_feedback(host.feedbacks, timing);
                if self.engine.scene.is_tree_dirty(host.root) {
                    self.arm_repaint(host.root);
                }
            }
            crate::scene::service::FramePacing::RetryableFailure => {
                for (surface, callbacks) in host.callbacks {
                    self.pending_callbacks
                        .entry(surface)
                        .or_default()
                        .extend(callbacks);
                }
                for (surface, feedbacks) in host.feedbacks {
                    self.pending_presentation
                        .entry(surface)
                        .or_default()
                        .extend(feedbacks);
                }
                self.repaint_surface(host.root);
            }
            crate::scene::service::FramePacing::TerminalFailure => {
                for feedback in host.feedbacks.into_values().flatten() {
                    feedback.discarded();
                }
                self.repaint_surface(host.root);
            }
            crate::scene::service::FramePacing::Pending
            | crate::scene::service::FramePacing::Skipped => return,
        }
        hl_debug!(
            tag::PRESENT,
            "settle async root={} submission={} pacing={:?}",
            host.root.0,
            completion.id.0,
            frame.pacing
        );
    }

    fn tree_protocol_surfaces(&self, root: SurfaceId) -> Vec<SurfaceId> {
        self.pending_callbacks
            .keys()
            .chain(self.pending_presentation.keys())
            .copied()
            .filter(|&surface| {
                self.engine.scene.window_root(surface) == Some(root) || surface == root
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn answer_host_feedback(
        &self,
        feedbacks: HashMap<SurfaceId, Vec<PresentationFeedbackCallback>>,
        timing: Option<crate::scene::port::PresentTiming>,
    ) {
        let present_ns = timing
            .map(|timing| timing.present_ns)
            .or_else(crate::scene::port::clock::monotonic_nanos);
        let Some(present_ns) = present_ns else {
            for feedback in feedbacks.into_values().flatten() {
                feedback.discarded();
            }
            return;
        };
        let now = std::time::Duration::from_nanos(present_ns);
        let refresh_ns = timing
            .map(|timing| timing.refresh_ns)
            .filter(|refresh| *refresh > 0)
            .unwrap_or_else(|| {
                self.engine
                    .scene
                    .primary_output()
                    .map(|output| output.refresh_nanos())
                    .unwrap_or(16_666_667)
            });
        let refresh = Refresh::fixed(std::time::Duration::from_nanos(refresh_ns));
        for (surface, callbacks) in feedbacks {
            let root = self.engine.scene.window_root(surface).unwrap_or(surface);
            let output = self
                .entered_outputs
                .get(&root)
                .copied()
                .or_else(|| {
                    self.engine
                        .scene
                        .selected_output(root)
                        .map(|output| output.id)
                })
                .and_then(|id| self.wl_output_handle(id))
                .or_else(|| self.primary_wl_output());
            for feedback in callbacks {
                if let Some(output) = output {
                    let kind = timing
                        .filter(|timing| timing.vsync)
                        .map_or(wp_presentation_feedback::Kind::empty(), |_| {
                            wp_presentation_feedback::Kind::Vsync
                        });
                    feedback.presented(output, now, refresh, 0, kind);
                } else {
                    feedback.discarded();
                }
            }
        }
    }

    /// Record that `root` owes a repaint at its next refresh boundary (or immediately, if it has never
    /// presented yet — a first present that failed retryably). Earlier deadlines win.
    pub(super) fn arm_repaint(&mut self, root: SurfaceId) {
        if self.engine.scene.window_state(root).is_none() {
            self.pending_repaints.remove(&root);
            return;
        }
        let now = self.engine.clock().now_nanos();
        let refresh = self
            .engine
            .scene
            .selected_output(root)
            .map(|output| output.refresh_nanos())
            .unwrap_or(16_666_667);
        let due = repaint_deadline(now, self.engine.next_present_due_ns(root), refresh);
        self.pending_repaints
            .entry(root)
            .and_modify(|d| *d = (*d).min(due))
            .or_insert(due);
    }

    pub(crate) fn repaint_surface(&mut self, surface: SurfaceId) {
        let root = self.engine.scene.window_root(surface).unwrap_or(surface);
        self.engine.scene.mark_dirty(root);
        self.arm_repaint(root);
    }

    /// The earliest host-monotonic deadline (ns) at which a repaint is owed, if any — the serve loop
    /// clamps its next wait to this so a throttled frame ships promptly rather than a fixed tick late.
    pub fn next_repaint_deadline(&self) -> Option<u64> {
        self.pending_repaints.values().copied().min()
    }

    /// Re-drive `present_root` for every window root whose repaint deadline has arrived, releasing the
    /// callbacks of any frame that now ships. Called by the serve loop each iteration. A root whose
    /// present is STILL not due (a clock that has not advanced a full interval) is left armed for a later
    /// tick rather than busy-looped.
    pub fn drive_due_repaints(&mut self) {
        let now = self.engine.clock().now_nanos();
        let due = take_due_repaints(&mut self.pending_repaints, now);
        for root in due {
            // Only surfaces that still exist and still root a window can present.
            if self.engine.scene.window_state(root).is_none() {
                self.pending_callbacks.remove(&root);
                continue;
            }
            let frame = self.engine.present_root(root);
            if frame.throttled {
                // Not actually due yet (deadline race with a non-monotonic clock read): leave armed.
                self.arm_repaint(root);
            } else {
                self.settle_frame(root, &frame);
            }
        }
    }

    /// Fire (and remove) the frame callbacks held for every surface whose window root is `root` — the
    /// whole presented tree (root + subsurfaces + popups all resolve to it via `window_root`).
    pub(super) fn fire_tree_callbacks(&mut self, root: SurfaceId) {
        let time_ms = (self.engine.clock().now_nanos() / 1_000_000) as u32;
        let targets: Vec<SurfaceId> = self
            .pending_callbacks
            .keys()
            .copied()
            .filter(|&sid| self.engine.scene.window_root(sid) == Some(root) || sid == root)
            .collect();
        for sid in targets {
            if let Some(callbacks) = self.pending_callbacks.remove(&sid) {
                for callback in callbacks {
                    callback.done(time_ms);
                }
            }
        }
    }

    /// Answer the `wp_presentation_feedback` callbacks held for `root`'s tree: `presented(now, refresh)`
    /// when `presented`, or `discarded` when the frame was torn down unshown. The timestamp is the best
    /// monotonic approximation available at this boundary and the refresh is the primary output's interval.
    ///
    /// The protocol sequence is the output's actual vertical-retrace counter, not a compositor-local
    /// content counter. Likewise `Kind::Vsync` promises evidence that the frame was shown at vertical
    /// retrace. The current presenter only proves that a drawable was enqueued, so report sequence zero and
    /// no flags until a native presented callback supplies the real timestamp, counter, and presentation
    /// mode. Honest unknown values keep clients from deriving a false display phase.
    pub(super) fn answer_tree_feedback(&mut self, root: SurfaceId, presented: bool) {
        if self.pending_presentation.is_empty() {
            return;
        }
        let targets: Vec<SurfaceId> = self
            .pending_presentation
            .keys()
            .copied()
            .filter(|&sid| self.engine.scene.window_root(sid) == Some(root) || sid == root)
            .collect();
        if targets.is_empty() {
            return;
        }
        let now = std::time::Duration::from_nanos(self.engine.clock().now_nanos());
        // Frame interval from the primary output's refresh (mHz → ns): 60_000 mHz ⇒ ~16.67 ms.
        let refresh_mhz = self
            .engine
            .scene
            .primary_output()
            .map(|o| o.refresh_mhz.max(1))
            .unwrap_or(60_000);
        let refresh = Refresh::fixed(std::time::Duration::from_nanos(
            1_000_000_000_000u64 / refresh_mhz as u64,
        ));
        for sid in targets {
            // Name the output this surface's frame presented on: its currently-entered output, else its
            // selected output, else the primary. Cloned (smithay's `Output` is an `Arc` handle) so the
            // `pending_presentation` mutation below does not conflict with the output borrow.
            let root = self.engine.scene.window_root(sid).unwrap_or(sid);
            let output_handle = self
                .entered_outputs
                .get(&root)
                .copied()
                .or_else(|| self.engine.scene.selected_output(root).map(|o| o.id))
                .and_then(|id| self.wl_output_handle(id))
                .or_else(|| self.primary_wl_output())
                .cloned();
            let Some(feedbacks) = self.pending_presentation.remove(&sid) else {
                continue;
            };
            for feedback in feedbacks {
                if presented {
                    if let Some(output_handle) = &output_handle {
                        feedback.presented(
                            output_handle,
                            now,
                            refresh,
                            0,
                            wp_presentation_feedback::Kind::empty(),
                        );
                    } else {
                        feedback.discarded();
                    }
                } else {
                    feedback.discarded();
                }
            }
        }
    }

    /// Fire (and remove) the frame callbacks held for a single surface.
    pub(super) fn fire_callbacks_for(&mut self, sid: SurfaceId) {
        let time_ms = (self.engine.clock().now_nanos() / 1_000_000) as u32;
        if let Some(callbacks) = self.pending_callbacks.remove(&sid) {
            for callback in callbacks {
                callback.done(time_ms);
            }
        }
    }

    /// Drop (without firing) the frame callbacks held for `root`'s tree — a terminally-failed frame.
    pub(super) fn drop_tree_callbacks(&mut self, root: SurfaceId) {
        let targets: Vec<SurfaceId> = self
            .pending_callbacks
            .keys()
            .copied()
            .filter(|&sid| self.engine.scene.window_root(sid) == Some(root) || sid == root)
            .collect();
        for sid in targets {
            self.pending_callbacks.remove(&sid);
        }
    }
}

/// Preserve an exact future throttle boundary, but pace a failed/offscreen retry no sooner than the next
/// output refresh. A stale `last_present + refresh` deadline must not become a permanent due-now loop.
fn repaint_deadline(now: u64, present_due: Option<u64>, refresh: u64) -> u64 {
    match present_due {
        Some(due) if due > now => due,
        Some(_) | None => now.saturating_add(refresh.max(1)),
    }
}

/// Drain exactly the roots whose repaint boundary has arrived. Consuming the old deadline before the
/// present attempt lets a retryable result arm a new refresh boundary instead of preserving stale state.
fn take_due_repaints(
    pending: &mut std::collections::HashMap<SurfaceId, u64>,
    now: u64,
) -> Vec<SurfaceId> {
    let due = pending
        .iter()
        .filter_map(|(&root, &deadline)| (deadline <= now).then_some(root))
        .collect::<Vec<_>>();
    for root in &due {
        pending.remove(root);
    }
    due
}

#[cfg(test)]
#[cfg(test)]
include!("pacing.rs");
