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
        }
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
        if self.canceled.contains(&key) {
            return (Defer::Waiting, vec![deferred]);
        }
        if self.poisoned.contains(&key.token)
            || self.closing.contains(&key.token)
            || !self.activate(key.token)
        {
            return (Defer::Waiting, vec![deferred]);
        }
        if self.last_joined.get(&key.token).copied() == Some(key.serial) {
            return (Defer::Reuse(deferred), Vec::new());
        }
        if let Some(frame) = self.frames.remove(&key) {
            self.frame_order.retain(|candidate| *candidate != key);
            self.last_joined.insert(key.token, key.serial);
            self.activate(key.token);
            return (Defer::Ready(Ready { frame, deferred }), Vec::new());
        }
        if self
            .last_joined
            .get(&key.token)
            .is_some_and(|last| key.serial < *last)
        {
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
            return (Defer::Waiting, vec![deferred]);
        }
        queue.push_back(Pending::commit(deferred));
        for key in matching {
            let Some(frame) = self.frames.remove(&key) else {
                continue;
            };
            self.frame_order.retain(|candidate| *candidate != key);
            match self.resolve_pending(frame) {
                PendingFrame::Ready(ready) => return (Defer::Ready(*ready), Vec::new()),
                PendingFrame::Discarded => {}
                PendingFrame::Unmatched(frame) => {
                    self.frame_order.push_back(key);
                    self.frames.insert(key, frame);
                }
            }
        }
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
