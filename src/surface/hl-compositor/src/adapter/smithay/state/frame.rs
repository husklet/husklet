use super::*;

impl HlState {
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

    /// Record that `root` owes a repaint at its next refresh boundary (or immediately, if it has never
    /// presented yet — a first present that failed retryably). Earlier deadlines win.
    pub(super) fn arm_repaint(&mut self, root: SurfaceId) {
        if self.engine.scene.window_state(root).is_none() {
            self.pending_repaints.remove(&root);
            return;
        }
        let due = self
            .engine
            .next_present_due_ns(root)
            .unwrap_or_else(|| self.engine.clock().now_nanos());
        self.pending_repaints
            .entry(root)
            .and_modify(|d| *d = (*d).min(due))
            .or_insert(due);
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
        let due: Vec<SurfaceId> = self
            .pending_repaints
            .iter()
            .filter(|(_, &deadline)| now >= deadline)
            .map(|(&root, _)| root)
            .collect();
        for root in due {
            // Only surfaces that still exist and still root a window can present.
            if self.engine.scene.window_state(root).is_none() {
                self.pending_repaints.remove(&root);
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

    /// Answer the `wp_presentation_feedback` callbacks held for `root`'s tree: `presented(now, refresh,
    /// seq)` when `presented`, or `discarded` when the frame was torn down unshown. The timestamp is the
    /// host-monotonic clock (`CLOCK_MONOTONIC`, the id `wp_presentation` advertised), the refresh is the
    /// primary output's frame interval, and `seq` is the monotonic present counter — one increment per
    /// PRESENT CYCLE (all feedbacks released by this one present share the frame's `seq` + `now`), so a
    /// client sees a strictly increasing, contiguous sequence: one number per frame that reached the screen.
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
        // ONE presentation sequence number for THIS present cycle. Every feedback answered in this call
        // resolved against the SAME frame reaching the screen at the SAME timestamp `now` (a burst of
        // commits coalesced by the vsync throttle accumulates several feedbacks that all release on this one
        // present), so they must all carry the SAME `seq`: a `wp_presentation` sequence is a per-output
        // vblank counter — one frame is one number. Stamping each feedback with a distinct `seq` would
        // report several vblanks at one identical instant, which no real display can produce and which
        // corrupts a client's (Chrome's) vsync-phase estimate. Allocated lazily so a cycle that only
        // discards never advances the counter, which would otherwise leave a gap in the presented run.
        let mut frame_seq: Option<u64> = None;
        for sid in targets {
            // Name the output this surface's frame presented on: its currently-entered output, else its
            // selected output, else the primary. Cloned (smithay's `Output` is an `Arc` handle) so the
            // `present_seq` / `pending_presentation` mutations below don't conflict with the borrow.
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
                        // Allocate this cycle's sequence number on first real present, then reuse it for
                        // every remaining feedback in the cycle (same frame ⇒ same seq + same `now`).
                        let seq = match frame_seq {
                            Some(s) => s,
                            None => {
                                self.present_seq += 1;
                                frame_seq = Some(self.present_seq);
                                self.present_seq
                            }
                        };
                        feedback.presented(
                            output_handle,
                            now,
                            refresh,
                            seq,
                            wp_presentation_feedback::Kind::Vsync,
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
