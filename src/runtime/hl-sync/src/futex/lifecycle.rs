use std::sync::atomic::Ordering;

use super::FutexTable;
use crate::{FutexKey, FutexSnapshot};

impl FutexTable {
    pub(super) fn remove_waiter(&self, identifier: u64, decrement: bool) -> bool {
        let mut buckets = self
            .buckets
            .iter()
            .map(|bucket| bucket.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            .collect::<Vec<_>>();
        let removed = buckets
            .iter_mut()
            .fold(false, |removed, bucket| bucket.remove_identifier(identifier) || removed);
        drop(buckets);
        if removed && decrement {
            self.waiter_count.fetch_sub(1, Ordering::AcqRel);
        }
        removed
    }

    pub fn reset_private_process(&self, process: u64) -> usize {
        self.remove_where(|key| matches!(key, FutexKey::Private { process: owner, .. } if owner == process))
    }

    pub fn reset_private(&self) -> usize {
        self.remove_where(|key| matches!(key, FutexKey::Private { .. }))
    }

    pub fn snapshot(&self) -> FutexSnapshot {
        let mut waits = Vec::new();
        for bucket in &self.buckets {
            let bucket = bucket.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            waits.extend(bucket.snapshots());
        }
        waits.sort_by_key(|wait| (wait.key, wait.bitset));
        FutexSnapshot {
            next_waiter: self.next_waiter.load(Ordering::Acquire),
            waits,
        }
    }

    fn remove_where(&self, predicate: impl Fn(FutexKey) -> bool) -> usize {
        let mut removed = Vec::new();
        for bucket in &self.buckets {
            let mut bucket = bucket.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            removed.extend(bucket.drain_matching(&predicate));
        }
        self.waiter_count.fetch_sub(removed.len(), Ordering::AcqRel);
        for waiter in &removed {
            waiter.queue.notify_one();
        }
        removed.len()
    }
}
