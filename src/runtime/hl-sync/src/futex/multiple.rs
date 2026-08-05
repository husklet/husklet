use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hl_time::Clock;

use super::{FutexTable, FutexWaiter};
use crate::{
    FutexDeadline, FutexError, FutexWaitMultipleOutcome, FutexWaitTarget, Interruption, WaitOutcome, WaitQueue,
};

impl FutexTable {
    pub fn wait_multiple<C: Clock + ?Sized>(
        &self,
        targets: &[FutexWaitTarget],
        deadline: Option<FutexDeadline>,
        interruption: &Interruption,
        clock: &C,
    ) -> Result<FutexWaitMultipleOutcome, FutexError> {
        if targets.is_empty() {
            return Err(FutexError::InvalidArgument);
        }
        let deadline = Self::monotonic_deadline(deadline, clock)?;
        let waiter = self.register_multiple(targets)?;
        let outcome = waiter
            .queue
            .wait(0, interruption, deadline, clock)
            .map_err(|_| FutexError::ClockFailed)?;
        let removed = self.remove_waiter(waiter.identifier, false);
        let winner = waiter.winner.load(Ordering::Acquire);
        if removed && winner == usize::MAX {
            self.waiter_count.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(match outcome {
            _ if winner != usize::MAX => FutexWaitMultipleOutcome::Woken(winner),
            WaitOutcome::Notified | WaitOutcome::Interrupted => FutexWaitMultipleOutcome::Interrupted,
            WaitOutcome::TimedOut => FutexWaitMultipleOutcome::TimedOut,
        })
    }

    fn register_multiple(&self, targets: &[FutexWaitTarget]) -> Result<Arc<FutexWaiter>, FutexError> {
        if self.waiter_count.fetch_add(1, Ordering::AcqRel) >= self.limits.waiters {
            self.waiter_count.fetch_sub(1, Ordering::AcqRel);
            return Err(FutexError::ResourceLimit);
        }
        let identifier = self.next_waiter.fetch_add(1, Ordering::Relaxed);
        let queue = Arc::new(WaitQueue::new());
        let winner = Arc::new(AtomicUsize::new(usize::MAX));
        let registrations = targets
            .iter()
            .enumerate()
            .map(|(vector_index, _)| {
                Arc::new(FutexWaiter {
                    identifier,
                    bitset: u32::MAX,
                    queue: queue.clone(),
                    vector_index,
                    winner: winner.clone(),
                })
            })
            .collect::<Vec<_>>();
        let mut publish = || self.publish_multiple(targets, &registrations);
        let compared = self.memory.compare_apply_many(targets, &mut publish);
        match compared {
            Ok(None) => Ok(registrations[0].clone()),
            Ok(Some(_)) => {
                self.waiter_count.fetch_sub(1, Ordering::AcqRel);
                Err(FutexError::ValueMismatch)
            }
            Err(error) => {
                self.waiter_count.fetch_sub(1, Ordering::AcqRel);
                Err(error)
            }
        }
    }

    fn publish_multiple(
        &self,
        targets: &[FutexWaitTarget],
        registrations: &[Arc<FutexWaiter>],
    ) -> Result<(), FutexError> {
        let mut bucket_indices = targets
            .iter()
            .map(|target| self.bucket_index(target.key))
            .collect::<Vec<_>>();
        bucket_indices.sort_unstable();
        bucket_indices.dedup();
        let mut buckets = bucket_indices
            .iter()
            .map(|index| self.buckets[*index].lock().unwrap_or_else(|error| error.into_inner()))
            .collect::<Vec<_>>();
        let mut keys = targets.iter().map(|target| target.key).collect::<Vec<_>>();
        keys.sort_unstable();
        keys.dedup();
        for bucket_index in &bucket_indices {
            let index = bucket_indices.binary_search(bucket_index).expect("locked bucket");
            let additions = keys
                .iter()
                .filter(|key| self.bucket_index(**key) == *bucket_index && !buckets[index].queues.contains_key(key))
                .count();
            if buckets[index].queues.len().saturating_add(additions) > self.limits.keys {
                return Err(FutexError::ResourceLimit);
            }
        }
        for (target, registration) in targets.iter().zip(registrations) {
            let index = bucket_indices
                .binary_search(&self.bucket_index(target.key))
                .expect("locked bucket");
            buckets[index]
                .queues
                .entry(target.key)
                .or_default()
                .push_back(registration.clone());
        }
        Ok(())
    }
}
