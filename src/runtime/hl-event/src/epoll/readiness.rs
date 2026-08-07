//! Readiness refresh, wakeups, and retirement of watches.
use crate::epoll::{Epoll, EpollError, EpollInner, EpollState, Watch};
use hl_descriptor::{ObjectError, Readiness, ReadinessObserver, ReadinessSubscription};
use std::sync::Arc;
impl Epoll {
    pub(crate) fn validate_active(state: &EpollState) -> Result<(), EpollError> {
        if state.retired {
            Err(EpollError::Retired)
        } else {
            Ok(())
        }
    }

    pub(crate) fn refresh_token(inner: &Arc<EpollInner>, token: u64) {
        let snapshot = {
            let state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return;
            }
            let Some(watch) = state.token_index.get(&token).map(|index| &state.watches[*index]) else {
                return;
            };
            (watch.target.clone(), watch.interests, watch.observed, watch.disabled)
        };
        let (target, interests, observed, disabled) = snapshot;
        let sampled = target.readiness(interests.readiness());
        let mut state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = state.token_index.get(&token).copied() else {
            return;
        };
        let transition = Readiness::from_bits(sampled.bits() & !observed.bits()).bits() != 0;
        let should_queue = !disabled && sampled.bits() != 0 && (!interests.edge_triggered() || transition);
        state.watches[index].observed = sampled;
        if should_queue && !state.watches[index].queued {
            state.watches[index].queued = true;
            state.ready.push_back(token);
        }
        if should_queue {
            state.watches[index].readiness_sequence = state.watches[index].readiness_sequence.wrapping_add(1).max(1);
        }
        Self::wake(inner, &mut state);
        drop(state);
        inner.readiness.notify();
    }

    pub(crate) fn signal_token(inner: &Arc<EpollInner>, token: u64) {
        let mut state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired {
            return;
        }
        let Some(index) = state.token_index.get(&token).copied() else {
            return;
        };
        if state.watches[index].disabled {
            return;
        }
        state.watches[index].readiness_sequence = state.watches[index].readiness_sequence.wrapping_add(1).max(1);
        if !state.watches[index].queued {
            state.watches[index].queued = true;
            state.ready.push_back(token);
        }
        Self::wake(inner, &mut state);
        drop(state);
        inner.readiness.notify();
    }

    pub(crate) fn prune_retired(&self) {
        let removed = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::take_retired(&mut state)
        };
        for watch in removed {
            watch.subscription.quiesce();
        }
    }

    pub(crate) fn take_retired(state: &mut EpollState) -> Vec<Watch> {
        let mut removed = Vec::new();
        let mut index = 0;
        while index < state.watches.len() {
            if !state.watches[index].target.retired() {
                index += 1;
                continue;
            }
            let watch = state.watches.remove(index);
            state.ready.retain(|token| *token != watch.token);
            removed.push(watch);
        }
        state.token_index = state
            .watches
            .iter()
            .enumerate()
            .map(|(index, watch)| (watch.token, index))
            .collect();
        removed
    }

    pub(crate) fn reindex_from(state: &mut EpollState, first: usize) {
        for (index, watch) in state.watches.iter().enumerate().skip(first) {
            state.token_index.insert(watch.token, index);
        }
    }

    pub(crate) fn wake(inner: &EpollInner, state: &mut EpollState) {
        state.epoch = state.epoch.wrapping_add(1);
        inner.changed.notify_all();
    }

    pub(crate) fn subscribe_observer(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.inner.readiness.subscribe(observer)
    }

    pub(crate) fn retire_description(&self) {
        let watches = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return;
            }
            state.retired = true;
            state.epoch = state.epoch.wrapping_add(1);
            self.inner.changed.notify_all();
            let watches = std::mem::take(&mut state.watches);
            state.token_index.clear();
            watches
        };
        self.inner
            .retired_watches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(watches);
    }

    /// Completes final close after descriptor retirement has stopped new waits.
    /// Every target subscription is synchronously quiesced exactly once.
    pub fn finish_retirement(&self) {
        let watches = std::mem::take(
            &mut *self
                .inner
                .retired_watches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for watch in watches {
            watch.subscription.quiesce();
        }
        self.inner.readiness.close();
    }
}
