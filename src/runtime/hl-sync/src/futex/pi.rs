use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use hl_time::{Clock, Deadline};

use crate::{
    FutexClock, FutexDeadline, FutexError, FutexKey, FutexLimits, FutexMemory, Interruption, WaitOutcome, WaitQueue,
};

pub const FUTEX_WAITERS: u32 = 0x8000_0000;
pub const FUTEX_OWNER_DIED: u32 = 0x4000_0000;
pub const FUTEX_TID_MASK: u32 = 0x3fff_ffff;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiFutexOutcome {
    Acquired,
    Interrupted,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiFutexError {
    Deadlock,
    Permission,
    WouldBlock,
    Fault,
    ClockFailed,
    ResourceLimit,
}

struct PiWaiter<O> {
    owner: O,
    queue: Arc<WaitQueue>,
}

struct PiWait<O> {
    waiter: Arc<PiWaiter<O>>,
    observation: u64,
}

enum PiAttempt<O> {
    Acquired,
    Retry,
    Wait(PiWait<O>),
}

struct PiState<O> {
    owner: Option<O>,
    waiters: VecDeque<Arc<PiWaiter<O>>>,
}

pub struct PiFutexTable<O> {
    memory: Arc<dyn FutexMemory>,
    states: Mutex<BTreeMap<FutexKey, PiState<O>>>,
    number: Arc<dyn Fn(O) -> u32 + Send + Sync>,
    limits: FutexLimits,
}

impl<O: Copy + Eq + Send + Sync + 'static> PiFutexTable<O> {
    pub fn new(
        memory: Arc<dyn FutexMemory>,
        number: impl Fn(O) -> u32 + Send + Sync + 'static,
        limits: FutexLimits,
    ) -> Self {
        Self {
            memory,
            states: Mutex::new(BTreeMap::new()),
            number: Arc::new(number),
            limits,
        }
    }

    /// # Errors
    ///
    /// Returns [`PiFutexError`] when deadline conversion, memory access,
    /// ownership validation, or waiter registration fails.
    pub fn lock<C: Clock + ?Sized>(
        &self,
        key: FutexKey,
        caller: O,
        observed_owner: Option<O>,
        try_only: bool,
        deadline: Option<FutexDeadline>,
        interruption: &Interruption,
        clock: &C,
    ) -> Result<PiFutexOutcome, PiFutexError> {
        let deadline = Self::monotonic_deadline(deadline, clock)?;
        let mut waiter = None;
        loop {
            let wait = match self.try_acquire(key, caller, observed_owner, try_only, waiter.as_ref())? {
                PiAttempt::Acquired => return Ok(PiFutexOutcome::Acquired),
                PiAttempt::Retry => continue,
                PiAttempt::Wait(wait) => wait,
            };
            waiter = Some(wait.waiter.clone());
            let outcome = wait
                .waiter
                .queue
                .wait(wait.observation, interruption, deadline, clock)
                .map_err(|_| PiFutexError::ClockFailed)?;
            match outcome {
                WaitOutcome::Notified => {}
                WaitOutcome::Interrupted => {
                    self.remove_waiter(key, caller);
                    return Ok(PiFutexOutcome::Interrupted);
                }
                WaitOutcome::TimedOut => {
                    self.remove_waiter(key, caller);
                    return Ok(PiFutexOutcome::TimedOut);
                }
            }
        }
    }

    /// # Errors
    ///
    /// Returns [`PiFutexError`] when the caller does not own the futex, memory
    /// access fails, or the key limit is exhausted.
    pub fn unlock(&self, key: FutexKey, caller: O) -> Result<(), PiFutexError> {
        let mut states = self.states.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !states.contains_key(&key) && states.len() >= self.limits.keys {
            return Err(PiFutexError::ResourceLimit);
        }
        let state = states.entry(key).or_insert_with(|| PiState {
            owner: None,
            waiters: VecDeque::new(),
        });
        let observed = self.memory.load(key).map_err(Self::memory_error)?;
        if observed & FUTEX_TID_MASK != (self.number)(caller) || state.owner.is_some_and(|owner| owner != caller) {
            return Err(PiFutexError::Permission);
        }
        // The futex word is Linux's ownership authority. A racing userspace
        // unlock can make a contending syscall observe zero and retire the
        // cached owner while an already-dispatched UNLOCK_PI still names the
        // same owner in the word. Reconstitute that transient cache entry so
        // the syscall can perform the handoff instead of stranding waiters.
        state.owner = Some(caller);
        let replacement = if state.waiters.is_empty() { 0 } else { FUTEX_WAITERS };
        if self
            .memory
            .compare_exchange(key, observed, replacement)
            .map_err(Self::memory_error)?
            != observed
        {
            return Err(PiFutexError::Fault);
        }
        state.owner = None;
        if let Some(waiter) = state.waiters.front() {
            waiter.queue.notify_one();
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns [`PiFutexError`] when an owned futex word cannot be read or updated.
    pub fn owner_exit(&self, owner: O) -> Result<usize, PiFutexError> {
        let mut states = self.states.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut released = 0;
        for (key, state) in states.iter_mut() {
            if state.owner != Some(owner) {
                continue;
            }
            let observed = self.memory.load(*key).map_err(Self::memory_error)?;
            if observed & FUTEX_TID_MASK != (self.number)(owner) && observed & FUTEX_OWNER_DIED == 0 {
                // An uncontended PI unlock may complete entirely in userspace,
                // leaving no FUTEX_UNLOCK_PI call through which to retire our
                // cached owner. OWNER_DIED with no owner is different: robust
                // cleanup published it immediately before this PI handoff.
                state.owner = None;
                continue;
            }
            let replacement = (observed & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
            if self
                .memory
                .compare_exchange(*key, observed, replacement)
                .map_err(Self::memory_error)?
                != observed
            {
                return Err(PiFutexError::Fault);
            }
            state.owner = None;
            if let Some(waiter) = state.waiters.front() {
                waiter.queue.notify_one();
            }
            released += 1;
        }
        Ok(released)
    }

    /// # Errors
    ///
    /// Returns [`PiFutexError`] when a private futex owner word cannot be cleared.
    pub fn reset_private(&self) -> Result<usize, PiFutexError> {
        let mut states = self.states.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let keys = states
            .keys()
            .copied()
            .filter(|key| matches!(key, FutexKey::Private { .. }))
            .collect::<Vec<_>>();
        let removed = keys.len();
        for key in keys {
            self.clear_owner_word(key)?;
            if let Some(state) = states.remove(&key) {
                Self::notify_waiters(state);
            }
        }
        Ok(removed)
    }

    #[must_use]
    pub fn is_quiescent(&self) -> bool {
        let states = self.states.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        states
            .values()
            .all(|state| state.owner.is_none() && state.waiters.is_empty())
    }

    fn try_acquire(
        &self,
        key: FutexKey,
        caller: O,
        observed_owner: Option<O>,
        try_only: bool,
        registered: Option<&Arc<PiWaiter<O>>>,
    ) -> Result<PiAttempt<O>, PiFutexError> {
        let mut states = self.states.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !states.contains_key(&key) && states.len() >= self.limits.keys {
            return Err(PiFutexError::ResourceLimit);
        }
        let waiter_count = states.values().map(|state| state.waiters.len()).sum::<usize>();
        let state = states.entry(key).or_insert_with(|| PiState {
            owner: observed_owner,
            waiters: VecDeque::new(),
        });
        let observed = self.memory.load(key).map_err(Self::memory_error)?;
        let word_owner = observed & FUTEX_TID_MASK;
        if state.owner.is_none_or(|owner| (self.number)(owner) != word_owner) {
            state.owner = observed_owner.filter(|owner| (self.number)(*owner) == word_owner);
        }
        if state.owner == Some(caller) || observed & FUTEX_TID_MASK == (self.number)(caller) {
            return Err(PiFutexError::Deadlock);
        }
        let at_front =
            registered.is_some_and(|waiter| state.waiters.front().is_some_and(|front| Arc::ptr_eq(front, waiter)));
        // Observe while the PI state lock still excludes unlock and
        // owner-death notification. Capturing after returning from this
        // predicate section loses a handoff delivered before `wait` registers.
        let observation = registered.map(|waiter| waiter.queue.observation());
        if (state.waiters.is_empty() || at_front) && self.acquire_available(key, state, caller, observed, at_front)? {
            return Ok(PiAttempt::Acquired);
        }
        if try_only {
            return Err(PiFutexError::WouldBlock);
        }
        if let Some(waiter) = registered {
            return Ok(PiAttempt::Wait(PiWait {
                waiter: waiter.clone(),
                observation: observation.expect("registered waiter observation"),
            }));
        }
        if waiter_count >= self.limits.waiters {
            return Err(PiFutexError::ResourceLimit);
        }
        let waiter = Arc::new(PiWaiter {
            owner: caller,
            queue: Arc::new(WaitQueue::new()),
        });
        let observation = waiter.queue.observation();
        state.waiters.push_back(waiter.clone());
        if self.publish_waiters_bit(key)? {
            return Ok(PiAttempt::Wait(PiWait { waiter, observation }));
        }
        state.waiters.pop_back();
        Ok(PiAttempt::Retry)
    }

    fn acquire_available(
        &self,
        key: FutexKey,
        state: &mut PiState<O>,
        caller: O,
        mut observed: u32,
        at_front: bool,
    ) -> Result<bool, PiFutexError> {
        for _ in 0..8 {
            if observed & FUTEX_TID_MASK != 0 {
                return Ok(false);
            }
            let remaining = state.waiters.len().saturating_sub(usize::from(at_front));
            let replacement = (self.number)(caller)
                | (observed & FUTEX_OWNER_DIED)
                | (remaining > 0).then_some(FUTEX_WAITERS).unwrap_or_default();
            let current = self
                .memory
                .compare_exchange(key, observed, replacement)
                .map_err(Self::memory_error)?;
            if current == observed {
                Self::complete_acquisition(state, caller, at_front);
                return Ok(true);
            }
            observed = current;
        }
        Ok(false)
    }

    fn complete_acquisition(state: &mut PiState<O>, caller: O, at_front: bool) {
        if at_front {
            state.waiters.pop_front();
        }
        state.owner = Some(caller);
    }

    fn publish_waiters_bit(&self, key: FutexKey) -> Result<bool, PiFutexError> {
        let mut current = self.memory.load(key).map_err(Self::memory_error)?;
        for _ in 0..8 {
            if current & FUTEX_TID_MASK == 0 {
                return Ok(false);
            }
            if current & FUTEX_WAITERS != 0 {
                return Ok(true);
            }
            let expected = current;
            current = self
                .memory
                .compare_exchange(key, expected, expected | FUTEX_WAITERS)
                .map_err(Self::memory_error)?;
            if current == expected {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn clear_owner_word(&self, key: FutexKey) -> Result<(), PiFutexError> {
        let mut observed = self.memory.load(key).map_err(Self::memory_error)?;
        for _ in 0..8 {
            if observed == 0 {
                return Ok(());
            }
            let current = self
                .memory
                .compare_exchange(key, observed, 0)
                .map_err(Self::memory_error)?;
            if current == observed {
                return Ok(());
            }
            observed = current;
        }
        Err(PiFutexError::Fault)
    }

    fn notify_waiters(state: PiState<O>) {
        for waiter in state.waiters {
            waiter.queue.notify_one();
        }
    }

    fn remove_waiter(&self, key: FutexKey, owner: O) {
        let mut states = self.states.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = states.get_mut(&key) {
            let removed_front = state.waiters.front().is_some_and(|waiter| waiter.owner == owner);
            state.waiters.retain(|waiter| waiter.owner != owner);
            // Unlock grants the free word to the head of the precise FIFO. If
            // that waiter leaves because of a signal or deadline after the
            // grant, advance the notification instead of stranding its
            // successor behind an ownerless WAITERS word.
            if removed_front && let Some(waiter) = state.waiters.front() {
                waiter.queue.notify_one();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn waiter_observations(&self, key: FutexKey) -> Vec<u64> {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(|state| state.waiters.iter().map(|waiter| waiter.queue.observation()).collect())
            .unwrap_or_default()
    }

    fn memory_error(_: FutexError) -> PiFutexError {
        PiFutexError::Fault
    }

    fn monotonic_deadline<C: Clock + ?Sized>(
        deadline: Option<FutexDeadline>,
        clock: &C,
    ) -> Result<Option<Deadline>, PiFutexError> {
        let Some(deadline) = deadline else {
            return Ok(None);
        };
        let value = deadline.value.checked_nanoseconds().unwrap_or(u64::MAX);
        match deadline.clock {
            FutexClock::Monotonic => Ok(Some(Deadline::from_nanoseconds(value))),
            FutexClock::Realtime => {
                let realtime = clock
                    .realtime_now()
                    .map_err(|_| PiFutexError::ClockFailed)?
                    .checked_nanoseconds()
                    .unwrap_or(u64::MAX);
                let monotonic = clock
                    .monotonic_now()
                    .map_err(|_| PiFutexError::ClockFailed)?
                    .nanoseconds();
                Ok(Some(Deadline::from_nanoseconds(
                    monotonic.saturating_add(value.saturating_sub(realtime)),
                )))
            }
        }
    }
}
