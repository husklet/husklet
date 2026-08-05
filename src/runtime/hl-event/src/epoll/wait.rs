use std::collections::HashSet;
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use hl_descriptor::{CancellationNotification, OperationCancellation};

use super::{BatchSelection, Epoll, EpollBatch, EpollError, EpollEvent, EpollInner, EpollState};

struct WaitNotification(std::sync::Weak<EpollInner>);

impl CancellationNotification for WaitNotification {
    fn notify(&self) {
        if let Some(inner) = self.0.upgrade() {
            let mut state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
            state.epoch = state.epoch.wrapping_add(1);
            drop(state);
            inner.changed.notify_all();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitOutcome {
    Woken,
    TimedOut,
}

impl Epoll {
    pub fn sample(&self, maximum: usize) -> Result<Vec<EpollEvent>, EpollError> {
        loop {
            let batch = self.peek(maximum)?;
            let events = batch.events.clone();
            if self.commit(batch)? {
                return Ok(events);
            }
        }
    }

    pub fn peek(&self, maximum: usize) -> Result<EpollBatch, EpollError> {
        if maximum == 0 {
            return Err(EpollError::InvalidArgument);
        }
        self.prune_retired();
        let snapshots = {
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::validate_active(&state)?;
            state
                .ready
                .iter()
                .filter_map(|token| {
                    state
                        .token_index
                        .get(token)
                        .map(|index| &state.watches[*index])
                        .map(|watch| {
                            (
                                watch.token,
                                watch.revision,
                                watch.readiness_sequence,
                                watch.target.clone(),
                                watch.interests,
                                watch.data,
                                watch.disabled,
                            )
                        })
                })
                .collect::<Vec<_>>()
        };
        let mut events = Vec::with_capacity(maximum);
        let mut selections = Vec::with_capacity(maximum);
        for (token, revision, sequence, target, interests, data, disabled) in snapshots {
            if events.len() == maximum {
                break;
            }
            if disabled {
                continue;
            }
            let sampled = target.readiness(interests.readiness());
            if sampled.bits() == 0 {
                continue;
            }
            events.push(EpollEvent {
                readiness: sampled,
                data,
            });
            selections.push(BatchSelection {
                token,
                revision,
                readiness_sequence: sequence,
                sampled,
            });
        }
        Ok(EpollBatch { events, selections })
    }

    /// Commits exactly one previously observed batch.
    ///
    /// `Ok(false)` means a selected registration was modified or deleted and
    /// no selection was consumed.
    pub fn commit(&self, batch: EpollBatch) -> Result<bool, EpollError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::validate_active(&state)?;
        for selection in &batch.selections {
            let Some(watch) = state
                .token_index
                .get(&selection.token)
                .map(|index| &state.watches[*index])
            else {
                return Ok(false);
            };
            if watch.revision != selection.revision {
                return Ok(false);
            }
        }
        let mut consumed = Vec::with_capacity(batch.selections.len());
        for selection in batch.selections {
            if let Some(requeue) = Self::commit_selection(&mut state, selection) {
                consumed.push((selection.token, requeue));
            }
        }
        if !consumed.is_empty() {
            let tokens = consumed.iter().map(|(token, _)| *token).collect::<HashSet<_>>();
            state.ready.retain(|token| !tokens.contains(token));
            state.ready.extend(
                consumed
                    .into_iter()
                    .filter_map(|(token, requeue)| requeue.then_some(token)),
            );
        }
        drop(state);
        if !batch.events.is_empty() {
            self.inner.readiness.notify();
        }
        Ok(true)
    }

    fn commit_selection(state: &mut EpollState, selection: BatchSelection) -> Option<bool> {
        let index = state
            .token_index
            .get(&selection.token)
            .copied()
            .expect("validated selection remains present");
        let watch = &mut state.watches[index];
        watch.observed = selection.sampled;
        let oneshot = watch.interests.oneshot();
        let unchanged = watch.readiness_sequence == selection.readiness_sequence;
        let level = !watch.interests.edge_triggered();
        if oneshot {
            watch.disabled = true;
            watch.queued = false;
        } else if unchanged {
            watch.queued = level;
        } else {
            return None;
        }
        Some(level && !oneshot)
    }

    pub fn wait(&self, maximum: usize, timeout: Option<Duration>) -> Result<Vec<EpollEvent>, EpollError> {
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        loop {
            let batch = self.peek_wait_until(maximum, timeout, deadline)?;
            let events = batch.events.clone();
            if self.commit(batch)? {
                return Ok(events);
            }
        }
    }

    pub fn peek_wait(&self, maximum: usize, timeout: Option<Duration>) -> Result<EpollBatch, EpollError> {
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        self.peek_wait_until(maximum, timeout, deadline)
    }

    pub fn peek_wait_interruptible(
        &self,
        maximum: usize,
        timeout: Option<Duration>,
        cancellation: &dyn OperationCancellation,
    ) -> Result<EpollBatch, EpollError> {
        let notification = std::sync::Arc::new(WaitNotification(std::sync::Arc::downgrade(&self.inner)));
        let _subscription = cancellation.subscribe(notification);
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        self.peek_wait_interrupt(maximum, timeout, deadline, cancellation)
    }

    fn peek_wait_interrupt(
        &self,
        maximum: usize,
        timeout: Option<Duration>,
        deadline: Option<Instant>,
        cancellation: &dyn OperationCancellation,
    ) -> Result<EpollBatch, EpollError> {
        loop {
            if cancellation.interrupted() {
                return Err(EpollError::Interrupted);
            }
            let epoch = {
                let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
                Self::validate_active(&state)?;
                state.epoch
            };
            let batch = self.peek(maximum)?;
            if !batch.events.is_empty() || timeout == Some(Duration::ZERO) {
                return Ok(batch);
            }
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::validate_active(&state)?;
            if state.epoch != epoch {
                continue;
            }
            if cancellation.interrupted() {
                return Err(EpollError::Interrupted);
            }
            if self.wait_for_change(state, deadline)? == WaitOutcome::TimedOut {
                return Ok(batch);
            }
        }
    }

    fn peek_wait_until(
        &self,
        maximum: usize,
        timeout: Option<Duration>,
        deadline: Option<Instant>,
    ) -> Result<EpollBatch, EpollError> {
        loop {
            let epoch = {
                let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
                Self::validate_active(&state)?;
                state.epoch
            };
            let batch = self.peek(maximum)?;
            if !batch.events.is_empty() || timeout == Some(Duration::ZERO) {
                return Ok(batch);
            }
            let state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
            Self::validate_active(&state)?;
            if state.epoch != epoch {
                continue;
            }
            if self.wait_for_change(state, deadline)? == WaitOutcome::TimedOut {
                return Ok(batch);
            }
        }
    }

    fn wait_for_change(
        &self,
        state: MutexGuard<'_, EpollState>,
        deadline: Option<Instant>,
    ) -> Result<WaitOutcome, EpollError> {
        let Some(deadline) = deadline else {
            let state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
            Self::validate_active(&state)?;
            return Ok(WaitOutcome::Woken);
        };
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(WaitOutcome::TimedOut);
        };
        let (state, result) = self
            .inner
            .changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|error| error.into_inner());
        Self::validate_active(&state)?;
        Ok(if result.timed_out() {
            WaitOutcome::TimedOut
        } else {
            WaitOutcome::Woken
        })
    }
}
