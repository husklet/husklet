//! Safe host-neutral wait queues and interruption.
//!
//! A waiter registers while holding the queue state lock. Notification updates
//! the sequence and selects registered waiters under that same lock, closing
//! the notify-before-sleep race. Blocking uses condition variables and never
//! spins.

#![forbid(unsafe_code)]

use hl_time::{ClockError, Deadline, MonotonicClock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration as StdDuration;

mod futex;

pub use futex::model::{
    FutexAtomicOperation, FutexClock, FutexDeadline, FutexError, FutexKey, FutexLimits, FutexSnapshot,
    FutexWaitMultipleOutcome, FutexWaitOutcome, FutexWaitSnapshot, FutexWaitTarget,
};
pub use futex::pi::{FUTEX_OWNER_DIED, FUTEX_TID_MASK, FUTEX_WAITERS, PiFutexError, PiFutexOutcome, PiFutexTable};
pub use futex::{FutexMemory, FutexTable};

/// Result of a wait operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitOutcome {
    /// The queue was notified after the caller's observation.
    Notified,
    /// The waiter's one-shot interruption was consumed.
    Interrupted,
    /// The absolute monotonic deadline elapsed.
    TimedOut,
}

/// Errors reading the injected monotonic clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitError {
    /// The injected clock failed.
    Clock(ClockError),
}

impl From<ClockError> for WaitError {
    fn from(error: ClockError) -> Self {
        Self::Clock(error)
    }
}

#[derive(Debug, Default)]
struct WaiterState {
    notified: bool,
}

#[derive(Debug, Default)]
struct Waiter {
    state: Mutex<WaiterState>,
    wake: Condvar,
}

impl Waiter {
    fn notify(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.notified {
            return false;
        }
        state.notified = true;
        self.wake.notify_one();
        true
    }

    fn signal(&self) {
        // Taking the waiter's mutex makes the pending-flag check and entry into
        // Condvar::wait atomic with respect to this wake.
        let _state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        self.wake.notify_one();
    }

    fn wait<'wait>(&'wait self, state: MutexGuard<'wait, WaiterState>) -> MutexGuard<'wait, WaiterState> {
        self.wake.wait(state).unwrap_or_else(|error| error.into_inner())
    }

    fn wait_for<'wait>(
        &'wait self,
        state: MutexGuard<'wait, WaiterState>,
        duration: StdDuration,
    ) -> MutexGuard<'wait, WaiterState> {
        self.wake
            .wait_timeout(state, duration)
            .unwrap_or_else(|error| error.into_inner())
            .0
    }

    fn wait_until<'wait, C: MonotonicClock + ?Sized>(
        &'wait self,
        state: MutexGuard<'wait, WaiterState>,
        deadline: Option<Deadline>,
        clock: &C,
    ) -> Result<(MutexGuard<'wait, WaiterState>, bool), WaitError> {
        let Some(deadline) = deadline else {
            return Ok((self.wait(state), false));
        };
        let now = clock.monotonic_now()?;
        if deadline.has_elapsed_at(now) {
            return Ok((state, true));
        }
        let remaining = deadline.remaining_at(now).nanoseconds();
        Ok((self.wait_for(state, StdDuration::from_nanos(remaining)), false))
    }
}

#[derive(Debug, Default)]
struct QueueState {
    sequence: u64,
    waiters: Vec<Weak<Waiter>>,
}

/// A sequence-based wait queue.
///
/// Callers first obtain [`observation`](Self::observation), inspect their
/// domain predicate, then call [`wait`](Self::wait) with that observation if
/// the predicate is still false. A notification between those steps changes
/// the sequence, so `wait` returns without blocking.
#[derive(Debug, Default)]
pub struct WaitQueue {
    state: Mutex<QueueState>,
}

impl WaitQueue {
    /// Creates an empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                sequence: 0,
                waiters: Vec::new(),
            }),
        }
    }

    /// Captures the current notification sequence.
    #[must_use]
    pub fn observation(&self) -> u64 {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).sequence
    }

    /// Wakes one currently registered waiter.
    ///
    /// The sequence changes even when no waiter is registered, preserving a
    /// notification for a caller that already captured an older observation.
    /// The return value is the number of waiters actually selected: zero or
    /// one.
    pub fn notify_one(&self) -> usize {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.sequence = state.sequence.wrapping_add(1);
        let mut selected = 0;
        state.waiters.retain(|weak| {
            let Some(waiter) = weak.upgrade() else {
                return false;
            };
            if selected == 0 && waiter.notify() {
                selected = 1;
            }
            true
        });
        selected
    }

    /// Wakes every currently registered waiter.
    ///
    /// Returns the number of waiters actually selected.
    pub fn notify_all(&self) -> usize {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.sequence = state.sequence.wrapping_add(1);
        let mut selected = 0;
        state.waiters.retain(|weak| {
            let Some(waiter) = weak.upgrade() else {
                return false;
            };
            selected += usize::from(waiter.notify());
            true
        });
        selected
    }

    /// Blocks until notification, interruption, or an optional absolute
    /// monotonic deadline.
    ///
    /// A deadline equal to or earlier than the current clock reading polls and
    /// returns [`WaitOutcome::TimedOut`]. Spurious condition-variable wakeups
    /// recheck every condition and recompute the true deadline remainder.
    pub fn wait<C: MonotonicClock + ?Sized>(
        &self,
        observed: u64,
        interruption: &Interruption,
        deadline: Option<Deadline>,
        clock: &C,
    ) -> Result<WaitOutcome, WaitError> {
        let waiter = Arc::new(Waiter::default());

        {
            let mut queue = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if queue.sequence != observed {
                return Ok(WaitOutcome::Notified);
            }
            queue.waiters.retain(|entry| entry.strong_count() != 0);
            queue.waiters.push(Arc::downgrade(&waiter));
        }

        let queue_registration = QueueRegistration {
            queue: self,
            waiter: Arc::downgrade(&waiter),
        };
        let interruption_registration = interruption.register(&waiter);

        let outcome = {
            let mut state = waiter.state.lock().unwrap_or_else(|error| error.into_inner());
            loop {
                if state.notified {
                    break Ok(WaitOutcome::Notified);
                }
                if interruption.consume() {
                    break Ok(WaitOutcome::Interrupted);
                }

                let waited = waiter.wait_until(state, deadline, clock)?;
                state = waited.0;
                if waited.1 {
                    break Ok(WaitOutcome::TimedOut);
                }
            }
        };

        drop(interruption_registration);
        drop(queue_registration);
        outcome
    }

    fn unregister(&self, waiter: &Weak<Waiter>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .waiters
            .retain(|entry| !Weak::ptr_eq(entry, waiter) && entry.strong_count() != 0);
    }
}

struct QueueRegistration<'a> {
    queue: &'a WaitQueue,
    waiter: Weak<Waiter>,
}

impl Drop for QueueRegistration<'_> {
    fn drop(&mut self) {
        self.queue.unregister(&self.waiter);
    }
}

#[derive(Debug)]
struct InterruptionState {
    pending: AtomicBool,
    next_registration: AtomicU64,
    waiters: Mutex<Vec<(u64, Weak<Waiter>)>>,
    observers: Mutex<Vec<(u64, Weak<dyn InterruptionWake>)>>,
}

pub trait InterruptionWake: Send + Sync {
    fn wake(&self);
}

pub struct InterruptionObservation {
    state: Arc<InterruptionState>,
    identifier: u64,
}

/// A clonable, one-shot interruption for one logical waiter.
///
/// Calling [`interrupt`](Self::interrupt) repeatedly before a wait records one
/// interruption. The next wait consumes it. If the waiter is already blocked,
/// interruption wakes it immediately without polling.
#[derive(Clone, Debug)]
pub struct Interruption {
    state: Arc<InterruptionState>,
}

impl Default for Interruption {
    fn default() -> Self {
        Self::new()
    }
}

impl Interruption {
    /// Creates an untriggered interruption.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(InterruptionState {
                pending: AtomicBool::new(false),
                next_registration: AtomicU64::new(1),
                waiters: Mutex::new(Vec::new()),
                observers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Records an interruption and wakes a currently blocked waiter.
    pub fn interrupt(&self) {
        self.state.pending.store(true, Ordering::Release);
        let waiters: Vec<Arc<Waiter>> = {
            let mut entries = self.state.waiters.lock().unwrap_or_else(|error| error.into_inner());
            let mut live = Vec::new();
            entries.retain(|(_, weak)| {
                let Some(waiter) = weak.upgrade() else {
                    return false;
                };
                live.push(waiter);
                true
            });
            live
        };
        for waiter in waiters {
            waiter.signal();
        }
        let observers: Vec<Arc<dyn InterruptionWake>> = {
            let mut entries = self.state.observers.lock().unwrap_or_else(|error| error.into_inner());
            let mut live = Vec::new();
            entries.retain(|(_, weak)| {
                let Some(observer) = weak.upgrade() else {
                    return false;
                };
                live.push(observer);
                true
            });
            live
        };
        for observer in observers {
            observer.wake();
        }
    }

    /// Reports whether an interruption is pending without consuming it.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.state.pending.load(Ordering::Acquire)
    }

    /// Consumes one pending interruption for an external blocking adapter.
    pub fn take_pending(&self) -> bool {
        self.consume()
    }

    pub fn observe(&self, observer: Arc<dyn InterruptionWake>) -> InterruptionObservation {
        let identifier = self.state.next_registration.fetch_add(1, Ordering::Relaxed);
        self.state
            .observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((identifier, Arc::downgrade(&observer)));
        InterruptionObservation {
            state: self.state.clone(),
            identifier,
        }
    }

    fn consume(&self) -> bool {
        self.state.pending.swap(false, Ordering::AcqRel)
    }

    fn register(&self, waiter: &Arc<Waiter>) -> InterruptionRegistration {
        let identifier = self.state.next_registration.fetch_add(1, Ordering::Relaxed);
        self.state
            .waiters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((identifier, Arc::downgrade(waiter)));
        InterruptionRegistration {
            state: Arc::clone(&self.state),
            identifier,
        }
    }
}

impl Drop for InterruptionObservation {
    fn drop(&mut self) {
        self.state
            .observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|(identifier, weak)| *identifier != self.identifier && weak.strong_count() != 0);
    }
}

struct InterruptionRegistration {
    state: Arc<InterruptionState>,
    identifier: u64,
}

impl Drop for InterruptionRegistration {
    fn drop(&mut self) {
        self.state
            .waiters
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|(identifier, weak)| *identifier != self.identifier && weak.strong_count() != 0);
    }
}

#[cfg(test)]
mod test;
