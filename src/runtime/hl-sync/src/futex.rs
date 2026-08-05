use hl_time::{Clock, Deadline};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
pub(crate) mod model;
pub(crate) mod pi;

use self::model::{
    FutexAtomicOperation, FutexClock, FutexDeadline, FutexError, FutexKey, FutexLimits, FutexWaitOutcome,
    FutexWaitSnapshot, FutexWaitTarget,
};
use crate::{Interruption, WaitOutcome, WaitQueue};

mod lifecycle;
mod multiple;

pub trait FutexMemory: Send + Sync {
    /// # Errors
    ///
    /// Returns [`FutexError`] when the key cannot be read.
    fn load(&self, key: FutexKey) -> Result<u32, FutexError>;

    /// Compares the word and publishes the waiter while guest stores and wake
    /// observation are excluded by the memory owner's atomicity mechanism.
    ///
    /// # Errors
    ///
    /// Returns [`FutexError`] when the key cannot be read, the value does not
    /// match, or the supplied operation fails.
    fn compare_and_apply(
        &self,
        key: FutexKey,
        expected: u32,
        mismatch: FutexError,
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<(), FutexError>;

    /// # Errors
    ///
    /// Returns [`FutexError`] when a target cannot be read or the supplied
    /// operation fails.
    fn compare_apply_many(
        &self,
        targets: &[FutexWaitTarget],
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<Option<usize>, FutexError>;

    /// # Errors
    ///
    /// Returns [`FutexError`] when the key cannot be updated atomically.
    fn atomic_update(&self, key: FutexKey, operation: FutexAtomicOperation) -> Result<i32, FutexError>;

    /// # Errors
    ///
    /// Returns [`FutexError`] when the key cannot be compared and updated atomically.
    fn compare_exchange(&self, key: FutexKey, expected: u32, replacement: u32) -> Result<u32, FutexError>;
}

struct FutexWaiter {
    identifier: u64,
    bitset: u32,
    queue: Arc<WaitQueue>,
    vector_index: usize,
    winner: Arc<AtomicUsize>,
}

#[derive(Default)]
struct FutexBucket {
    queues: BTreeMap<FutexKey, VecDeque<Arc<FutexWaiter>>>,
}

impl FutexBucket {
    fn snapshots(&self) -> Vec<FutexWaitSnapshot> {
        self.queues
            .iter()
            .flat_map(|(key, queue)| {
                queue.iter().map(|waiter| FutexWaitSnapshot {
                    key: *key,
                    bitset: waiter.bitset,
                })
            })
            .collect()
    }

    fn remove_identifier(&mut self, identifier: u64) -> bool {
        let mut removed = false;
        self.queues.retain(|_, queue| {
            let before = queue.len();
            queue.retain(|waiter| waiter.identifier != identifier);
            removed |= before != queue.len();
            !queue.is_empty()
        });
        removed
    }

    fn drain_matching(&mut self, predicate: &impl Fn(FutexKey) -> bool) -> Vec<Arc<FutexWaiter>> {
        let keys: Vec<_> = self.queues.keys().copied().filter(|key| predicate(*key)).collect();
        let mut removed = Vec::new();
        for key in keys {
            if let Some(mut queue) = self.queues.remove(&key) {
                removed.extend(queue.drain(..));
            }
        }
        removed
    }
}

pub struct FutexTable {
    memory: Arc<dyn FutexMemory>,
    limits: FutexLimits,
    buckets: Vec<Mutex<FutexBucket>>,
    waiter_count: AtomicUsize,
    next_waiter: AtomicU64,
}

impl std::fmt::Debug for FutexTable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FutexTable")
            .field("limits", &self.limits)
            .field("waiters", &self.waiter_count.load(Ordering::Acquire))
            .finish()
    }
}

impl FutexTable {
    /// # Errors
    ///
    /// Returns `FutexError::InvalidArgument` for zero limits, or
    /// `FutexError::ResourceLimit` when bucket storage cannot be reserved.
    pub fn new(limits: FutexLimits, memory: Arc<dyn FutexMemory>) -> Result<Self, FutexError> {
        if limits.buckets == 0 || limits.keys == 0 || limits.waiters == 0 {
            return Err(FutexError::InvalidArgument);
        }
        let mut buckets = Vec::new();
        buckets
            .try_reserve_exact(limits.buckets)
            .map_err(|_| FutexError::ResourceLimit)?;
        for _ in 0..limits.buckets {
            buckets.push(Mutex::new(FutexBucket::default()));
        }
        Ok(Self {
            memory,
            limits,
            buckets,
            waiter_count: AtomicUsize::new(0),
            next_waiter: AtomicU64::new(1),
        })
    }

    /// # Errors
    ///
    /// Returns [`FutexError`] when the request is invalid, memory access or
    /// waiter registration fails, or the clock cannot be read.
    pub fn wait<C: Clock + ?Sized>(
        &self,
        key: FutexKey,
        expected: u32,
        bitset: u32,
        deadline: Option<FutexDeadline>,
        interruption: &Interruption,
        clock: &C,
    ) -> Result<FutexWaitOutcome, FutexError> {
        if bitset == 0 {
            return Err(FutexError::InvalidArgument);
        }
        let deadline = Self::monotonic_deadline(deadline, clock)?;
        let waiter = self.register(key, expected, bitset)?;
        let outcome = waiter
            .queue
            .wait(0, interruption, deadline, clock)
            .map_err(|_| FutexError::ClockFailed)?;
        self.remove_waiter(waiter.identifier, true);
        Ok(match outcome {
            WaitOutcome::Notified => FutexWaitOutcome::Woken,
            WaitOutcome::Interrupted => FutexWaitOutcome::Interrupted,
            WaitOutcome::TimedOut => FutexWaitOutcome::TimedOut,
        })
    }

    /// # Errors
    ///
    /// Returns `FutexError::InvalidArgument` when `bitset` is zero.
    pub fn wake(&self, key: FutexKey, count: usize, bitset: u32) -> Result<usize, FutexError> {
        if bitset == 0 {
            return Err(FutexError::InvalidArgument);
        }
        let selected = {
            let mut bucket = self
                .bucket(key)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::take_matching(&mut bucket, key, count, bitset)
        };
        self.waiter_count.fetch_sub(selected.len(), Ordering::AcqRel);
        for waiter in &selected {
            waiter.queue.notify_one();
        }
        Ok(selected.len())
    }

    /// # Errors
    ///
    /// Returns [`FutexError`] when the keys are identical, comparison or
    /// memory access fails, or the destination exceeds its key limit.
    pub fn requeue(
        &self,
        source: FutexKey,
        target: FutexKey,
        wake_count: usize,
        requeue_count: usize,
        compare: Option<u32>,
    ) -> Result<usize, FutexError> {
        if source == target {
            return Err(FutexError::InvalidArgument);
        }
        if let Some(expected) = compare {
            let mut result = None;
            let mut apply = || {
                result = Some(self.requeue_unchecked(source, target, wake_count, requeue_count)?);
                Ok(())
            };
            self.memory
                .compare_and_apply(source, expected, FutexError::CompareMismatch, &mut apply)?;
            return result.ok_or(FutexError::Fault);
        }
        self.requeue_unchecked(source, target, wake_count, requeue_count)
    }

    fn requeue_unchecked(
        &self,
        source: FutexKey,
        target: FutexKey,
        wake_count: usize,
        requeue_count: usize,
    ) -> Result<usize, FutexError> {
        let first_index = self.bucket_index(source).min(self.bucket_index(target));
        let second_index = self.bucket_index(source).max(self.bucket_index(target));
        if first_index == second_index {
            let mut bucket = self.buckets[first_index]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            return self.requeue_locked(&mut bucket, source, target, wake_count, requeue_count);
        }
        let mut first = self.buckets[first_index]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut second = self.buckets[second_index]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (source_bucket, target_bucket) = if self.bucket_index(source) == first_index {
            (&mut first, &mut second)
        } else {
            (&mut second, &mut first)
        };
        if !target_bucket.queues.contains_key(&target) && target_bucket.queues.len() >= self.limits.keys {
            return Err(FutexError::ResourceLimit);
        }
        let woken = Self::take_matching(source_bucket, source, wake_count, u32::MAX);
        let moved = Self::take_entries(source_bucket, source, requeue_count, u32::MAX);
        Self::append(target_bucket, target, moved, self.limits.keys)?;
        drop(second);
        drop(first);
        for waiter in &woken {
            waiter.queue.notify_one();
        }
        self.waiter_count.fetch_sub(woken.len(), Ordering::AcqRel);
        Ok(woken.len())
    }

    /// # Errors
    ///
    /// Returns [`FutexError`] when the atomic memory update fails.
    pub fn wake_op(
        &self,
        first: FutexKey,
        second: FutexKey,
        first_count: usize,
        second_count: usize,
        operation: FutexAtomicOperation,
        predicate: impl FnOnce(i32) -> bool,
    ) -> Result<usize, FutexError> {
        let old = self.memory.atomic_update(second, operation)?;
        let first_index = self.bucket_index(first);
        let second_index = self.bucket_index(second);
        let selected = if first_index == second_index {
            let mut bucket = self.buckets[first_index]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut selected = Self::take_matching(&mut bucket, first, first_count, u32::MAX);
            if predicate(old) {
                selected.extend(Self::take_matching(&mut bucket, second, second_count, u32::MAX));
            }
            selected
        } else {
            let low_index = first_index.min(second_index);
            let high_index = first_index.max(second_index);
            let mut low = self.buckets[low_index]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut high = self.buckets[high_index]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (first_bucket, second_bucket) = if first_index == low_index {
                (&mut low, &mut high)
            } else {
                (&mut high, &mut low)
            };
            let mut selected = Self::take_matching(first_bucket, first, first_count, u32::MAX);
            if predicate(old) {
                selected.extend(Self::take_matching(second_bucket, second, second_count, u32::MAX));
            }
            selected
        };
        self.waiter_count.fetch_sub(selected.len(), Ordering::AcqRel);
        for waiter in &selected {
            waiter.queue.notify_one();
        }
        Ok(selected.len())
    }

    fn register(&self, key: FutexKey, expected: u32, bitset: u32) -> Result<Arc<FutexWaiter>, FutexError> {
        if self.waiter_count.fetch_add(1, Ordering::AcqRel) >= self.limits.waiters {
            self.waiter_count.fetch_sub(1, Ordering::AcqRel);
            return Err(FutexError::ResourceLimit);
        }
        let waiter = Arc::new(FutexWaiter {
            identifier: self.next_waiter.fetch_add(1, Ordering::Relaxed),
            bitset,
            queue: Arc::new(WaitQueue::new()),
            vector_index: 0,
            winner: Arc::new(AtomicUsize::new(usize::MAX)),
        });
        let mut publish = || {
            let mut bucket = self
                .bucket(key)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::append(&mut bucket, key, vec![Arc::clone(&waiter)], self.limits.keys)
        };
        if let Err(error) = self
            .memory
            .compare_and_apply(key, expected, FutexError::ValueMismatch, &mut publish)
        {
            self.waiter_count.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        Ok(waiter)
    }

    fn requeue_locked(
        &self,
        bucket: &mut FutexBucket,
        source: FutexKey,
        target: FutexKey,
        wake_count: usize,
        requeue_count: usize,
    ) -> Result<usize, FutexError> {
        let source_remaining = bucket
            .queues
            .get(&source)
            .map_or(0, VecDeque::len)
            .saturating_sub(wake_count.saturating_add(requeue_count));
        if source_remaining != 0 && !bucket.queues.contains_key(&target) && bucket.queues.len() >= self.limits.keys {
            return Err(FutexError::ResourceLimit);
        }
        let woken = Self::take_matching(bucket, source, wake_count, u32::MAX);
        let moved = Self::take_entries(bucket, source, requeue_count, u32::MAX);
        Self::append(bucket, target, moved, self.limits.keys)?;
        for waiter in &woken {
            waiter.queue.notify_one();
        }
        self.waiter_count.fetch_sub(woken.len(), Ordering::AcqRel);
        Ok(woken.len())
    }

    fn append(
        bucket: &mut FutexBucket,
        key: FutexKey,
        waiters: Vec<Arc<FutexWaiter>>,
        key_limit: usize,
    ) -> Result<(), FutexError> {
        if waiters.is_empty() {
            return Ok(());
        }
        if !bucket.queues.contains_key(&key) && bucket.queues.len() >= key_limit {
            return Err(FutexError::ResourceLimit);
        }
        bucket.queues.entry(key).or_default().extend(waiters);
        Ok(())
    }

    fn take_matching(bucket: &mut FutexBucket, key: FutexKey, count: usize, bitset: u32) -> Vec<Arc<FutexWaiter>> {
        let mut selected = Vec::new();
        while selected.len() < count {
            let entries = Self::take_entries(bucket, key, count - selected.len(), bitset);
            if entries.is_empty() {
                break;
            }
            selected.extend(entries.into_iter().filter(|waiter| Self::elect(waiter)));
        }
        selected
    }

    fn elect(waiter: &FutexWaiter) -> bool {
        waiter
            .winner
            .compare_exchange(usize::MAX, waiter.vector_index, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn take_entries(bucket: &mut FutexBucket, key: FutexKey, count: usize, bitset: u32) -> Vec<Arc<FutexWaiter>> {
        let mut selected = Vec::new();
        let Some(queue) = bucket.queues.get_mut(&key) else {
            return selected;
        };
        let mut index = 0;
        while index < queue.len() && selected.len() < count {
            if queue[index].bitset & bitset != 0 {
                selected.push(queue.remove(index).expect("indexed futex waiter"));
            } else {
                index += 1;
            }
        }
        if queue.is_empty() {
            bucket.queues.remove(&key);
        }
        selected
    }

    fn monotonic_deadline<C: Clock + ?Sized>(
        deadline: Option<FutexDeadline>,
        clock: &C,
    ) -> Result<Option<Deadline>, FutexError> {
        let Some(deadline) = deadline else {
            return Ok(None);
        };
        let value = deadline.value.checked_nanoseconds().unwrap_or(u64::MAX);
        match deadline.clock {
            FutexClock::Monotonic => Ok(Some(Deadline::from_nanoseconds(value))),
            FutexClock::Realtime => {
                let realtime = clock
                    .realtime_now()
                    .map_err(|_| FutexError::ClockFailed)?
                    .checked_nanoseconds()
                    .unwrap_or(u64::MAX);
                let monotonic = clock
                    .monotonic_now()
                    .map_err(|_| FutexError::ClockFailed)?
                    .nanoseconds();
                Ok(Some(Deadline::from_nanoseconds(
                    monotonic.saturating_add(value.saturating_sub(realtime)),
                )))
            }
        }
    }

    fn bucket(&self, key: FutexKey) -> &Mutex<FutexBucket> {
        &self.buckets[self.bucket_index(key)]
    }

    fn bucket_index(&self, key: FutexKey) -> usize {
        let value = match key {
            FutexKey::Private { process, address } => process ^ address.rotate_left(17),
            FutexKey::Shared { backing, offset } => backing ^ offset.rotate_left(17),
        };
        usize::try_from(value % self.buckets.len() as u64).unwrap()
    }
}

#[cfg(test)]
mod lifecycle_test;
#[cfg(test)]
mod pi_test;
#[cfg(test)]
mod test;
#[cfg(test)]
mod test_support;
