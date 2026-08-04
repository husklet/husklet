use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_time::{ClockError, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};

use crate::{FutexAtomicOperation, FutexError, FutexKey, FutexLimits, FutexMemory, FutexTable, FutexWaitTarget};

#[derive(Debug, Default)]
pub(super) struct Memory {
    words: Mutex<BTreeMap<FutexKey, u32>>,
}

impl Memory {
    pub(super) fn store(&self, key: FutexKey, value: u32) {
        self.words
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key, value);
    }
}

impl FutexMemory for Memory {
    fn load(&self, key: FutexKey) -> Result<u32, FutexError> {
        self.words
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .copied()
            .ok_or(FutexError::Fault)
    }

    fn compare_and_apply(
        &self,
        key: FutexKey,
        expected: u32,
        mismatch: FutexError,
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<(), FutexError> {
        let values = self.words.lock().unwrap();
        if values.get(&key).copied().unwrap_or_default() != expected {
            return Err(mismatch);
        }
        apply()
    }

    fn compare_apply_many(
        &self,
        targets: &[FutexWaitTarget],
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<Option<usize>, FutexError> {
        let values = self.words.lock().unwrap_or_else(|error| error.into_inner());
        for (index, target) in targets.iter().enumerate() {
            let Some(observed) = values.get(&target.key) else {
                return Err(FutexError::Fault);
            };
            if *observed != target.expected {
                return Ok(Some(index));
            }
        }
        apply()?;
        Ok(None)
    }

    fn atomic_update(&self, key: FutexKey, operation: FutexAtomicOperation) -> Result<i32, FutexError> {
        let mut words = self.words.lock().unwrap_or_else(|error| error.into_inner());
        let word = words.get_mut(&key).ok_or(FutexError::Fault)?;
        let old = *word as i32;
        let next = match operation {
            FutexAtomicOperation::Set(value) => value,
            FutexAtomicOperation::Add(value) => old.wrapping_add(value),
            FutexAtomicOperation::Or(value) => old | value,
            FutexAtomicOperation::AndNot(value) => old & !value,
            FutexAtomicOperation::Xor(value) => old ^ value,
        };
        *word = next as u32;
        Ok(old)
    }

    fn compare_exchange(&self, key: FutexKey, expected: u32, replacement: u32) -> Result<u32, FutexError> {
        let mut words = self.words.lock().unwrap_or_else(|error| error.into_inner());
        let word = words.get_mut(&key).ok_or(FutexError::Fault)?;
        let observed = *word;
        if observed == expected {
            *word = replacement;
        }
        Ok(observed)
    }
}

pub(super) struct Clock {
    pub(super) monotonic: u64,
    pub(super) realtime: u64,
}

impl MonotonicClock for Clock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(self.monotonic))
    }
}

impl RealtimeClock for Clock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        Ok(Timespec::from_nanoseconds(self.realtime))
    }
}

pub(super) fn fixture() -> (Arc<FutexTable>, Arc<Memory>, FutexKey, FutexKey) {
    let memory = Arc::new(Memory::default());
    let first = FutexKey::private(7, 0x1000).unwrap();
    let second = FutexKey::shared(9, 0x2000).unwrap();
    memory.store(first, 3);
    memory.store(second, 5);
    let table = Arc::new(FutexTable::new(FutexLimits::default(), memory.clone()).unwrap());
    (table, memory, first, second)
}

impl FutexTable {
    pub(super) fn wait_until_registered(&self, count: usize) {
        while self.snapshot().waits.len() != count {
            std::thread::yield_now();
        }
    }
}
