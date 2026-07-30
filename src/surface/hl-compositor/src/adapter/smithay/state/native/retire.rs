use super::*;

impl NativeState {
    pub(super) fn external_is_stale(&self, token: NonZeroU64, serial: NonZeroU64) -> bool {
        self.last_joined
            .get(&token)
            .is_some_and(|last| serial <= *last)
    }

    pub(super) fn ingest(&mut self, mut frame: NativeFrame) -> Option<Ready> {
        let key = Key {
            token: frame.token,
            serial: frame.serial,
        };
        if self.canceled.remove(&key) {
            self.canceled_order.retain(|candidate| *candidate != key);
            frame.complete(NativeFrameOutcome::Discarded);
            return None;
        }
        if self.poisoned.contains(&key.token) {
            frame.complete(NativeFrameOutcome::Discarded);
            return None;
        }
        if !self.active_tokens.contains(&key.token) {
            frame.complete(NativeFrameOutcome::StaleToken);
            return None;
        }
        if let Some(last) = self.last_frame.get(&key.token).copied() {
            if key.serial == last {
                frame.complete(NativeFrameOutcome::Duplicate);
                return None;
            }
            if key.serial < last {
                frame.complete(NativeFrameOutcome::Decreasing);
                return None;
            }
        }
        self.last_frame.insert(key.token, key.serial);
        if let Some(deferred) = self.commits.remove(&key) {
            self.commit_order.retain(|candidate| *candidate != key);
            self.last_joined.insert(key.token, key.serial);
            let ready = Ready { frame, deferred };
            self.finalize_if_idle(key.token);
            return Some(ready);
        }
        match self.resolve_pending(frame) {
            PendingFrame::Ready(ready) => return Some(*ready),
            PendingFrame::Discarded => return None,
            PendingFrame::Unmatched(unmatched) => frame = unmatched,
        }
        let token_frames = self
            .frames
            .keys()
            .filter(|candidate| candidate.token == key.token)
            .count();
        if token_frames == self.serial_capacity {
            if let Some(index) = self
                .frame_order
                .iter()
                .position(|candidate| candidate.token == key.token)
            {
                let oldest = self
                    .frame_order
                    .remove(index)
                    .expect("index came from queue");
                if let Some(evicted) = self.frames.remove(&oldest) {
                    evicted.complete(NativeFrameOutcome::Capacity);
                }
            }
        }
        self.frame_order.push_back(key);
        self.frames.insert(key, frame);
        None
    }

    pub(super) fn take_ingress(
        &self,
    ) -> Result<Option<NativeFrame>, std::sync::mpsc::TryRecvError> {
        self.ingress.try_next()
    }

    pub(super) fn take_discarded(&mut self) -> Vec<Deferred> {
        std::mem::take(&mut self.discarded)
    }

    pub(super) fn take_cancellation(
        &self,
    ) -> Result<
        Option<crate::adapter::smithay::native::NativeFrameCancellation>,
        std::sync::mpsc::TryRecvError,
    > {
        self.ingress.try_cancel()
    }

    pub(super) fn take_before(&self, token: NonZeroU64, serial: NonZeroU64) -> Option<NativeFrame> {
        self.ingress.try_before(token, serial)
    }

    pub(super) fn cancel_key(&mut self, key: Key) -> Vec<Deferred> {
        self.frame_order.retain(|candidate| *candidate != key);
        if let Some(frame) = self.frames.remove(&key) {
            frame.complete(NativeFrameOutcome::TerminalFailure);
        }
        self.commit_order.retain(|candidate| *candidate != key);
        let mut deferred = self.commits.remove(&key).into_iter().collect::<Vec<_>>();
        if let Some(queue) = self.pending.remove(&key.token) {
            let mut kept = VecDeque::with_capacity(queue.len());
            for mut pending in queue {
                let exact = pending.published().is_some_and(|header| {
                    header.token == key.token.get() && header.serial == key.serial.get()
                });
                if exact {
                    pending.release_lease();
                    deferred.extend(pending.deferred);
                } else {
                    kept.push_back(pending);
                }
            }
            if !kept.is_empty() {
                self.pending.insert(key.token, kept);
            }
        }
        self.canceled.insert(key);
        self.canceled_order.push_back(key);
        while self.canceled_order.len() > self.token_capacity {
            if let Some(oldest) = self.canceled_order.pop_front() {
                self.canceled.remove(&oldest);
            }
        }
        self.finalize_if_idle(key.token);
        deferred
    }

    pub(super) fn disconnect(&mut self) -> Vec<Deferred> {
        for (_, frame) in self.frames.drain() {
            frame.complete(NativeFrameOutcome::Discarded);
        }
        self.frame_order.clear();
        self.commit_order.clear();
        self.last_frame.clear();
        self.last_joined.clear();
        self.active_tokens.clear();
        self.registrations.clear();
        self.closing.clear();
        self.poisoned.clear();
        self.canceled.clear();
        self.canceled_order.clear();
        self.commits
            .drain()
            .map(|(_, deferred)| deferred)
            .chain(self.discarded.drain(..))
            .chain(
                self.pending
                    .drain()
                    .flat_map(|(_, queue)| queue.into_iter().filter_map(Pending::into_deferred)),
            )
            .collect()
    }

    pub(super) fn activate(&mut self, token: NonZeroU64) -> bool {
        self.active_tokens.contains(&token)
            || (self.active_tokens.len() < self.token_capacity && self.active_tokens.insert(token))
    }

    pub(super) fn register(&mut self, token: NonZeroU64) -> bool {
        if self.poisoned.contains(&token) || !self.activate(token) {
            return false;
        }
        *self.registrations.entry(token).or_default() += 1;
        self.closing.remove(&token);
        true
    }

    pub(super) fn unregister(&mut self, token: NonZeroU64) {
        let Some(registrations) = self.registrations.get_mut(&token) else {
            return;
        };
        *registrations = registrations.saturating_sub(1);
        if *registrations > 0 {
            return;
        }
        self.registrations.remove(&token);
        self.closing.insert(token);
        self.finalize_if_idle(token);
    }

    pub(super) fn finalize_if_idle(&mut self, token: NonZeroU64) {
        let busy = self.commits.keys().any(|key| key.token == token)
            || self.pending.contains_key(&token)
            || self.frames.keys().any(|key| key.token == token);
        if self.closing.contains(&token) && !busy {
            self.closing.remove(&token);
            self.active_tokens.remove(&token);
            self.last_frame.remove(&token);
            self.last_joined.remove(&token);
            self.poisoned.remove(&token);
        }
    }

    /// Settle a terminal generation when its Wayland buffer is destroyed before the producer publishes a
    /// frame. Active generations retain their dma-buf lease: resource destruction and frame completion are
    /// independent, and a submitted frame may still arrive.
    pub(super) fn settle_destroyed(&mut self, token: NonZeroU64, external: &WeakDmabuf) {
        let mut remove = false;
        if let Some(queue) = self.pending.get_mut(&token) {
            queue.retain_mut(|pending| {
                if pending.terminal && pending.matches_external(external) {
                    pending.release_lease();
                    false
                } else {
                    true
                }
            });
            remove = queue.is_empty();
        }
        if remove {
            self.pending.remove(&token);
        }
        self.finalize_if_idle(token);
    }

    pub(super) fn cancel(&mut self, surface: SurfaceId) -> Vec<Deferred> {
        let keys = self
            .commits
            .iter()
            .filter_map(|(key, deferred)| (deferred.surface == surface).then_some(*key))
            .collect::<Vec<_>>();
        let mut deferred = Vec::with_capacity(keys.len());
        for key in keys {
            self.commit_order.retain(|candidate| *candidate != key);
            if let Some(commit) = self.commits.remove(&key) {
                deferred.push(commit);
            }
            self.canceled.insert(key);
            self.canceled_order.push_back(key);
        }
        for queue in self.pending.values_mut() {
            for slot in queue {
                if slot.surface() == surface {
                    slot.terminal = true;
                    if let Some(mut commit) = slot.deferred.take() {
                        slot.buffer = commit.buffer.take();
                        slot.external = commit.external.take();
                        deferred.push(commit);
                    }
                }
            }
        }
        while self.canceled_order.len() > self.token_capacity {
            if let Some(oldest) = self.canceled_order.pop_front() {
                self.canceled.remove(&oldest);
            }
        }
        deferred
    }

    /// Supersede deferred state for a surface that is immediately committing a replacement.
    ///
    /// Exact token/serial commits retain cancellation keys because their submitted frame can still arrive.
    /// Pending external generations retain their surface identity after supersession. A returning token may
    /// collapse only non-terminal generations owned by this same surface, and only when the token queue has
    /// no other surface. Terminal cancellation continues to use [`Self::cancel`] and never collapses.
    pub(super) fn replace(&mut self, surface: SurfaceId) -> Vec<Deferred> {
        let keys = self
            .commits
            .iter()
            .filter_map(|(key, deferred)| (deferred.surface == surface).then_some(*key))
            .collect::<Vec<_>>();
        let mut deferred = Vec::with_capacity(keys.len());
        for key in keys {
            self.commit_order.retain(|candidate| *candidate != key);
            if let Some(commit) = self.commits.remove(&key) {
                deferred.push(commit);
            }
            self.canceled.insert(key);
            self.canceled_order.push_back(key);
        }
        self.pending.retain(|_, queue| {
            queue.retain_mut(|slot| {
                if slot.surface != surface || slot.terminal {
                    return true;
                }
                if let Some(mut commit) = slot.deferred.take() {
                    slot.buffer = commit.buffer.take();
                    slot.external = commit.external.take();
                    deferred.push(commit);
                }
                true
            });
            !queue.is_empty()
        });
        while self.canceled_order.len() > self.token_capacity {
            if let Some(oldest) = self.canceled_order.pop_front() {
                self.canceled.remove(&oldest);
            }
        }
        deferred
    }

    pub(super) fn retire(&mut self, token: NonZeroU64) -> Vec<Deferred> {
        let frame_keys = self
            .frames
            .keys()
            .copied()
            .filter(|key| key.token == token)
            .collect::<Vec<_>>();
        for key in frame_keys {
            self.frame_order.retain(|candidate| *candidate != key);
            if let Some(frame) = self.frames.remove(&key) {
                frame.complete(NativeFrameOutcome::StaleToken);
            }
        }
        let commit_keys = self
            .commits
            .keys()
            .copied()
            .filter(|key| key.token == token)
            .collect::<Vec<_>>();
        let mut deferred = Vec::new();
        for key in commit_keys {
            self.commit_order.retain(|candidate| *candidate != key);
            if let Some(commit) = self.commits.remove(&key) {
                deferred.push(commit);
            }
        }
        deferred.extend(
            self.pending
                .remove(&token)
                .into_iter()
                .flat_map(|queue| queue.into_iter().filter_map(Pending::into_deferred)),
        );
        self.last_frame.remove(&token);
        self.last_joined.remove(&token);
        self.active_tokens.remove(&token);
        self.registrations.remove(&token);
        self.closing.remove(&token);
        self.poisoned.remove(&token);
        let canceled = self
            .canceled
            .iter()
            .copied()
            .filter(|key| key.token == token)
            .collect::<Vec<_>>();
        for key in canceled {
            self.canceled.remove(&key);
            self.canceled_order.retain(|candidate| *candidate != key);
        }
        deferred
    }
}
