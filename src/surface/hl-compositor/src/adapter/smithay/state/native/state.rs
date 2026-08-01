use super::*;
impl NativeState {
    pub(super) fn waiting_surface(&self, key: Key) -> Option<SurfaceId> {
        if let Some(deferred) = self.commits.get(&key) {
            return Some(deferred.surface);
        }
        let mut surfaces = self
            .pending
            .get(&key.token)?
            .iter()
            .filter_map(|pending| pending.deferred.as_ref().map(|_| pending.surface));
        let surface = surfaces.next()?;
        surfaces
            .all(|candidate| candidate == surface)
            .then_some(surface)
    }

    pub(super) fn new(ingress: NativeFrames) -> Self {
        let capacity = ingress.capacity();
        Self {
            ingress,
            frames: HashMap::new(),
            frame_order: VecDeque::new(),
            commits: HashMap::new(),
            commit_order: VecDeque::new(),
            pending: HashMap::new(),
            last_frame: HashMap::new(),
            last_joined: HashMap::new(),
            active_tokens: HashSet::new(),
            registrations: HashMap::new(),
            closing: HashSet::new(),
            poisoned: HashSet::new(),
            discarded: Vec::new(),
            canceled: HashSet::new(),
            canceled_order: VecDeque::new(),
            serial_capacity: capacity,
            token_capacity: capacity.saturating_mul(128).max(256),
            deferrals: Deferrals::default(),
            // Five seconds: long enough that a busy surface contributes one line every few seconds
            // rather than one per frame, short enough that a reader watching a stuck client sees the
            // outstanding count and its age climb while they are still watching.
            report: crate::diagnostic::Heartbeat::new(std::time::Duration::from_secs(5)),
        }
    }

    /// Record one deferral's fate, then report the running picture on a cadence.
    ///
    /// The report exists because `Defer::Waiting` had no voice at all: a commit parked forever and a
    /// commit never made produced the same silence, and reading that silence as "nothing is deferring"
    /// cost this fleet a day of bounding a defect. It is emitted at ERROR level so it survives a
    /// release build — the two `hl_debug!` lines in `defer` and `defer_pending` are compiled out of
    /// the build that ships, which is why they were no help — and on the `PRESENT` tag, which is the
    /// one an operator investigating presentation already enables.
    ///
    /// It proves its own liveness rather than reporting an absence: every line carries a positive
    /// total for each of the four fates WITH the window they accumulated over, so "no deferrals at
    /// all" and "deferrals that never complete" cannot print the same thing. `oldest_ms` is the age of
    /// the longest-outstanding park, which is the number that says the hypothesis is right.
    fn note(&mut self, key: Option<Key>, outcome: DeferOutcome, reason: &'static str) {
        // One error-level line per distinct (fate, reason), milestone-latched, carrying the surface and
        // token it happened to. This is what turns "parked=3 refused=3" into a sentence: which path
        // recorded it, and for a refusal, WHICH of the four refusals it was.
        if let Some(seen) = self.deferrals.reasons.record((outcome.name(), reason)) {
            hl_log::hl_log!(
                hl_log::tag::PRESENT,
                hl_log::Level::Error,
                "native deferral {} reason={} token={} serial={} count={} over_ms={}",
                outcome.name(),
                reason,
                key.map_or(0, |key| key.token.get()),
                key.map_or(0, |key| key.serial.get()),
                seen.count,
                seen.since.as_millis()
            );
        }
        match outcome {
            DeferOutcome::Joined => self.deferrals.joined += 1,
            DeferOutcome::Reused => self.deferrals.reused += 1,
            DeferOutcome::Refused => self.deferrals.refused += 1,
            DeferOutcome::Parked => {
                self.deferrals.parked += 1;
                if let Some(key) = key {
                    self.deferrals
                        .parked_at
                        .entry(key)
                        .or_insert_with(Instant::now);
                }
            }
        }
        self.report_deferrals();
    }

    /// The cadenced line. Also called from the frame drain so a client that stops committing entirely
    /// still has its outstanding parks reported — an instrument that only speaks when the subject acts
    /// goes quiet exactly when the subject wedges.
    pub(super) fn report_deferrals(&mut self) {
        // Pruned against the live tables rather than maintained at every removal site: a key that is
        // no longer parked cannot inflate `oldest_ms`, however its commit was retired.
        let commits = &self.commits;
        self.deferrals
            .parked_at
            .retain(|key, _| commits.contains_key(key));
        let outstanding = self.commits.len();
        let pending: usize = self.pending.values().map(VecDeque::len).sum();
        if outstanding == 0 && pending == 0 && self.deferrals.parked == 0 {
            // Nothing has ever been deferred and nothing is outstanding. Saying so every five seconds
            // would drown the log; the first deferral reports immediately, so silence here is bounded
            // by "no client has ever deferred a commit", which the counters state the moment one does.
            return;
        }
        let Some(beat) = self.report.record(()) else {
            return;
        };
        let oldest_ms = self
            .deferrals
            .parked_at
            .values()
            .map(|at| at.elapsed().as_millis())
            .max()
            .unwrap_or(0);
        hl_log::hl_log!(
            hl_log::tag::PRESENT,
            hl_log::Level::Error,
            "native deferrals joined={} reused={} parked={} refused={} outstanding={} \
             pending={} oldest_ms={} in {}ms ({} reports). Not a fault by itself: `parked` counts \
             commits waiting for their native frame and `joined` counts the ones that got it. \
             `outstanding` staying non-zero while `oldest_ms` climbs and `joined` does not is a \
             commit that will never complete, which is indistinguishable from one never made unless \
             this line exists.",
            self.deferrals.joined,
            self.deferrals.reused,
            self.deferrals.parked,
            self.deferrals.refused,
            outstanding,
            pending,
            oldest_ms,
            beat.window.as_millis(),
            beat.total
        );
    }

    pub(super) fn defer(&mut self, key: Key, deferred: Deferred) -> (Defer, Vec<Deferred>) {
        hl_log::hl_log!(
            hl_log::tag::COMPOSITOR,
            hl_log::Level::Debug,
            "native commit deferred surface={} token={} serial={} frame_ready={}",
            deferred.surface.0,
            key.token,
            key.serial,
            self.frames.contains_key(&key)
        );
        // Every exit below records WHICH fate it was before returning. Three of them return
        // `Defer::Waiting` while handing the commit back to be discarded — it is never parked and no
        // frame can ever complete it — and one returns `Defer::Waiting` having parked it. Collapsing
        // those into one value with no diagnostic is what made a wedged surface unreadable.
        if self.canceled.contains(&key) {
            self.note(Some(key), DeferOutcome::Refused, "defer:canceled");
            return (Defer::Waiting, vec![deferred]);
        }
        if self.poisoned.contains(&key.token)
            || self.closing.contains(&key.token)
            || !self.activate(key.token)
        {
            self.note(Some(key), DeferOutcome::Refused, "defer:token-not-live");
            return (Defer::Waiting, vec![deferred]);
        }
        if self.last_joined.get(&key.token).copied() == Some(key.serial) {
            self.note(Some(key), DeferOutcome::Reused, "defer:same-serial");
            return (Defer::Reuse(deferred), Vec::new());
        }
        if let Some(frame) = self.frames.remove(&key) {
            self.frame_order.retain(|candidate| *candidate != key);
            self.last_joined.insert(key.token, key.serial);
            self.activate(key.token);
            self.note(Some(key), DeferOutcome::Joined, "defer:frame-already-here");
            return (Defer::Ready(Ready { frame, deferred }), Vec::new());
        }
        if self
            .last_joined
            .get(&key.token)
            .is_some_and(|last| key.serial < *last)
        {
            self.note(Some(key), DeferOutcome::Refused, "defer:serial-overtaken");
            return (Defer::Waiting, vec![deferred]);
        }
        let mut discarded = self
            .commits
            .insert(key, deferred)
            .into_iter()
            .collect::<Vec<_>>();
        self.commit_order.retain(|candidate| *candidate != key);
        let token_commits = self
            .commits
            .keys()
            .filter(|candidate| candidate.token == key.token)
            .count();
        if token_commits > self.serial_capacity {
            if let Some(index) = self
                .commit_order
                .iter()
                .position(|candidate| candidate.token == key.token)
            {
                let oldest = self
                    .commit_order
                    .remove(index)
                    .expect("index came from queue");
                if let Some(commit) = self.commits.remove(&oldest) {
                    discarded.push(commit);
                }
            }
        }
        self.commit_order.push_back(key);
        self.note(Some(key), DeferOutcome::Parked, "defer:awaiting-frame");
        (Defer::Waiting, discarded)
    }

    /// Wait for the next produced frame of an already-validated external buffer.
    ///
    /// Chrome may commit a GBM buffer before its GPU thread reaches the synchronous host submission. The
    /// shared header therefore still carries serial zero (or the serial of that buffer's previous use).
    /// The token identifies the buffer exactly; the arriving native frame supplies the new serial. Only the
    /// The retained dma-buf header is re-read when a frame arrives; only an exact published token+serial
    /// joins. Superseded generations retain their buffer lease as tagged tombstones so a late exact frame is
    /// discarded instead of sliding to another commit.
    pub(super) fn defer_pending(
        &mut self,
        token: NonZeroU64,
        deferred: Deferred,
    ) -> (Defer, Vec<Deferred>) {
        hl_log::hl_log!(
            hl_log::tag::COMPOSITOR,
            hl_log::Level::Debug,
            "native commit deferred surface={} token={} serial=pending frame_ready={}",
            deferred.surface.0,
            token,
            self.frames.keys().any(|key| key.token == token)
        );
        if self.poisoned.contains(&token)
            || self.closing.contains(&token)
            || (!self.active_tokens.contains(&token) && deferred.external.is_none())
            || !self.activate(token)
        {
            self.note(None, DeferOutcome::Refused, "defer_pending:token-not-live");
            return (Defer::Waiting, vec![deferred]);
        }
        let matching = self
            .frames
            .keys()
            .copied()
            .filter(|key| key.token == token)
            .collect::<Vec<_>>();
        let queue = self.pending.entry(token).or_default();
        if queue.len() == self.serial_capacity {
            self.note(None, DeferOutcome::Refused, "defer_pending:queue-full");
            return (Defer::Waiting, vec![deferred]);
        }
        queue.push_back(Pending::commit(deferred));
        for key in matching {
            let Some(frame) = self.frames.remove(&key) else {
                continue;
            };
            self.frame_order.retain(|candidate| *candidate != key);
            match self.resolve_pending(frame) {
                PendingFrame::Ready(ready) => {
                    self.note(None, DeferOutcome::Joined, "defer_pending:frame-matched");
                    return (Defer::Ready(*ready), Vec::new());
                }
                PendingFrame::Discarded => {}
                PendingFrame::Unmatched(frame) => {
                    self.frame_order.push_back(key);
                    self.frames.insert(key, frame);
                }
            }
        }
        // Parked on the pending queue rather than on a `(token, serial)` key: the serial is not known
        // yet, so there is no key to age. `pending` in the report is what shows these.
        self.note(None, DeferOutcome::Parked, "defer_pending:awaiting-serial");
        (Defer::Waiting, Vec::new())
    }

    pub(super) fn resolve_pending(&mut self, frame: NativeFrame) -> PendingFrame {
        let key = Key {
            token: frame.token,
            serial: frame.serial,
        };
        let Some(queue) = self.pending.get(&key.token) else {
            return PendingFrame::Unmatched(frame);
        };
        let mut active = Vec::new();
        let mut tombstones = Vec::new();
        let mut newer = false;
        for (index, pending) in queue.iter().enumerate() {
            let Some(header) = pending.published() else {
                continue;
            };
            if header.token != key.token.get() {
                continue;
            }
            newer |= header.serial > key.serial.get();
            if header.serial != key.serial.get() {
                continue;
            }
            if pending.deferred.is_some() {
                active.push((index, pending.surface));
            } else {
                tombstones.push((index, pending.surface));
            }
        }

        if active.len() > 1
            || active
                .first()
                .is_some_and(|(_, surface)| tombstones.iter().any(|(_, owner)| owner != surface))
        {
            let frame_keys = self
                .frames
                .keys()
                .copied()
                .filter(|candidate| candidate.token == key.token)
                .collect::<Vec<_>>();
            for candidate in frame_keys {
                self.frame_order.retain(|queued| *queued != candidate);
                if let Some(queued) = self.frames.remove(&candidate) {
                    queued.complete(NativeFrameOutcome::Discarded);
                }
            }
            let commit_keys = self
                .commits
                .keys()
                .copied()
                .filter(|candidate| candidate.token == key.token)
                .collect::<Vec<_>>();
            for candidate in commit_keys {
                self.commit_order.retain(|queued| *queued != candidate);
                if let Some(deferred) = self.commits.remove(&candidate) {
                    self.discarded.push(deferred);
                }
            }
            if let Some(queue) = self.pending.remove(&key.token) {
                for mut pending in queue {
                    pending.release_lease();
                    if let Some(deferred) = pending.deferred {
                        self.discarded.push(deferred);
                    }
                }
            }
            // Keep registration ownership intact. This token is poisoned until its real owners retire it;
            // otherwise a colliding late frame could reactivate the same ambiguous identity.
            self.poisoned.insert(key.token);
            frame.complete(NativeFrameOutcome::Discarded);
            return PendingFrame::Discarded;
        }
        if let Some((selected, surface)) = active.first().copied() {
            let mut ready = None;
            let mut kept = VecDeque::with_capacity(queue.len());
            for (index, mut pending) in self
                .pending
                .remove(&key.token)
                .unwrap()
                .into_iter()
                .enumerate()
            {
                if index == selected {
                    ready = pending.deferred.take();
                    pending.release_lease();
                    continue;
                }
                let exact_tombstone = pending.deferred.is_none()
                    && pending.surface == surface
                    && pending.published().is_some_and(|header| {
                        header.token == key.token.get() && header.serial == key.serial.get()
                    });
                if exact_tombstone {
                    pending.release_lease();
                } else {
                    kept.push_back(pending);
                }
            }
            if !kept.is_empty() {
                self.pending.insert(key.token, kept);
            }
            let deferred = ready.expect("selected pending generation was active");
            self.last_joined.insert(key.token, key.serial);
            self.finalize_if_idle(key.token);
            return PendingFrame::Ready(Box::new(Ready { frame, deferred }));
        }
        if !tombstones.is_empty() {
            let mut kept = VecDeque::with_capacity(queue.len());
            for mut pending in self.pending.remove(&key.token).unwrap() {
                let exact = pending.deferred.is_none()
                    && pending.published().is_some_and(|header| {
                        header.token == key.token.get() && header.serial == key.serial.get()
                    });
                if exact {
                    pending.release_lease();
                } else {
                    kept.push_back(pending);
                }
            }
            if !kept.is_empty() {
                self.pending.insert(key.token, kept);
            }
            self.finalize_if_idle(key.token);
            frame.complete(NativeFrameOutcome::Discarded);
            return PendingFrame::Discarded;
        }
        if newer {
            frame.complete(NativeFrameOutcome::Decreasing);
            return PendingFrame::Discarded;
        }
        PendingFrame::Unmatched(frame)
    }
}
impl Drop for NativeState {
    fn drop(&mut self) {
        for (_, frame) in self.frames.drain() {
            frame.complete(NativeFrameOutcome::Discarded);
        }
        for (_, deferred) in self.commits.drain() {
            if let Some(buffer) = deferred.buffer {
                buffer.release();
            }
        }
        for deferred in self.discarded.drain(..) {
            if let Some(buffer) = deferred.buffer {
                buffer.release();
            }
        }
        for (_, queue) in self.pending.drain() {
            for mut pending in queue {
                pending.release_lease();
                let Some(deferred) = pending.deferred else {
                    continue;
                };
                if let Some(buffer) = deferred.buffer {
                    buffer.release();
                }
            }
        }
    }
}
