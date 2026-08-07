use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use hl_time::{ClockError, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};

use crate::{
    FUTEX_OWNER_DIED, FUTEX_TID_MASK, FUTEX_WAITERS, FutexAtomicOperation, FutexClock, FutexDeadline, FutexError,
    FutexKey, FutexMemory, FutexWaitTarget, Interruption, PiFutexError, PiFutexOutcome, PiFutexTable,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Owner {
    number: u32,
    generation: u16,
}

#[derive(Debug, Default)]
struct Memory {
    words: Mutex<BTreeMap<FutexKey, u32>>,
    release_on_waiters: AtomicBool,
    handoff_on_waiters: std::sync::atomic::AtomicU32,
}

impl Memory {
    fn store(&self, key: FutexKey, value: u32) {
        self.words.lock().unwrap().insert(key, value);
    }

    fn word(&self, key: FutexKey) -> u32 {
        self.words.lock().unwrap()[&key]
    }

    fn race_user_unlock(&self) {
        self.release_on_waiters.store(true, Ordering::Release);
    }

    /// Models an uncontended release plus the next acquisition, both completed
    /// in userspace while a waiter is publishing the waiters bit.
    fn race_user_handoff(&self, number: u32) {
        self.handoff_on_waiters.store(number, Ordering::Release);
    }
}

impl FutexMemory for Memory {
    fn load(&self, key: FutexKey) -> Result<u32, FutexError> {
        self.words.lock().unwrap().get(&key).copied().ok_or(FutexError::Fault)
    }

    fn compare_and_apply(
        &self,
        key: FutexKey,
        expected: u32,
        mismatch: FutexError,
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<(), FutexError> {
        if self.load(key)? != expected {
            return Err(mismatch);
        }
        apply()
    }

    fn compare_apply_many(
        &self,
        targets: &[FutexWaitTarget],
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<Option<usize>, FutexError> {
        for (index, target) in targets.iter().enumerate() {
            if self.load(target.key)? != target.expected {
                return Ok(Some(index));
            }
        }
        apply()?;
        Ok(None)
    }

    fn atomic_update(&self, _: FutexKey, _: FutexAtomicOperation) -> Result<i32, FutexError> {
        Err(FutexError::InvalidArgument)
    }

    fn compare_exchange(&self, key: FutexKey, expected: u32, replacement: u32) -> Result<u32, FutexError> {
        let mut words = self.words.lock().unwrap();
        let word = words.get_mut(&key).ok_or(FutexError::Fault)?;
        if replacement & FUTEX_WAITERS != 0 && expected & FUTEX_WAITERS == 0 {
            if self.release_on_waiters.swap(false, Ordering::AcqRel) {
                *word = 0;
            }
            let handoff = self.handoff_on_waiters.swap(0, Ordering::AcqRel);
            if handoff != 0 {
                *word = handoff;
            }
        }
        let observed = *word;
        if observed == expected {
            *word = replacement;
        }
        Ok(observed)
    }
}

struct Clock;

impl MonotonicClock for Clock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(0))
    }
}

impl RealtimeClock for Clock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        Ok(Timespec::ZERO)
    }
}

fn fixture() -> (Arc<PiFutexTable<Owner>>, Arc<Memory>, FutexKey) {
    let memory = Arc::new(Memory::default());
    let key = FutexKey::private(1, 0x1000).unwrap();
    memory.store(key, 0);
    (
        Arc::new(PiFutexTable::new(
            memory.clone(),
            |owner: Owner| owner.number,
            crate::FutexLimits::default(),
        )),
        memory,
        key,
    )
}

impl Owner {
    const fn new(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }
}

#[test]
fn contention_hands_off() {
    let (table, memory, key) = fixture();
    let first = Owner::new(10, 1);
    let second = Owner::new(11, 1);
    assert_eq!(
        table.lock(key, first, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    let waiter_table = table.clone();
    let waiter =
        thread::spawn(move || waiter_table.lock(key, second, Some(first), false, None, &Interruption::new(), &Clock));
    while memory.word(key) & FUTEX_WAITERS == 0 {
        thread::yield_now();
    }
    assert_eq!(table.unlock(key, first), Ok(()));
    assert_eq!(waiter.join().unwrap(), Ok(PiFutexOutcome::Acquired));
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, second.number);
    assert_eq!(table.unlock(key, second), Ok(()));
    assert_eq!(memory.word(key), 0);
}

// An uncontended release and the next acquisition both complete in userspace,
// so the cached owner names a thread that already handed ownership on. The
// word still names the caller, and the handoff must run from it.
#[test]
fn stale_cached_owner_still_hands_off() {
    let (table, memory, key) = fixture();
    let first = Owner::new(10, 1);
    let second = Owner::new(11, 1);
    let third = Owner::new(12, 1);
    assert_eq!(
        table.lock(key, first, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    memory.race_user_handoff(second.number);

    let waiter_table = table.clone();
    let waiter =
        thread::spawn(move || waiter_table.lock(key, third, Some(first), false, None, &Interruption::new(), &Clock));
    while memory.word(key) & FUTEX_WAITERS == 0 {
        thread::yield_now();
    }

    assert_eq!(table.unlock(key, second), Ok(()));
    assert_eq!(waiter.join().unwrap(), Ok(PiFutexOutcome::Acquired));
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, third.number);
}

#[test]
fn interrupted_front_advances_precise_queue() {
    let (table, memory, key) = fixture();
    let owner = Owner::new(10, 1);
    let first = Owner::new(11, 1);
    let second = Owner::new(12, 1);
    assert_eq!(
        table.lock(key, owner, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );

    let first_interruption = Arc::new(Interruption::new());
    let first_table = Arc::clone(&table);
    let first_signal = Arc::clone(&first_interruption);
    let first_waiter =
        thread::spawn(move || first_table.lock(key, first, Some(owner), false, None, &first_signal, &Clock));
    while table.waiter_observations(key).len() != 1 {
        thread::yield_now();
    }

    let second_table = Arc::clone(&table);
    let second_waiter =
        thread::spawn(move || second_table.lock(key, second, Some(owner), false, None, &Interruption::new(), &Clock));
    while table.waiter_observations(key).len() != 2 {
        thread::yield_now();
    }

    first_interruption.interrupt();
    assert_eq!(first_waiter.join().unwrap(), Ok(PiFutexOutcome::Interrupted));
    assert_eq!(table.waiter_observations(key).len(), 1);
    assert_eq!(table.unlock(key, owner), Ok(()));
    assert_eq!(second_waiter.join().unwrap(), Ok(PiFutexOutcome::Acquired));
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, second.number);
    assert_eq!(table.unlock(key, second), Ok(()));
}

#[test]
fn userspace_unlock_during_waiter_publication_retries_acquisition() {
    let (table, memory, key) = fixture();
    let first = Owner::new(10, 1);
    let second = Owner::new(11, 1);
    assert_eq!(
        table.lock(key, first, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );

    memory.race_user_unlock();
    assert_eq!(
        table.lock(key, second, Some(first), false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    assert_eq!(memory.word(key), second.number);
    assert_eq!(table.unlock(key, second), Ok(()));
}

#[test]
fn futex_word_restores_transiently_missing_owner_cache() {
    let (table, memory, key) = fixture();
    let owner = Owner::new(10, 1);
    assert_eq!(
        table.lock(key, owner, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );

    assert_eq!(table.owner_exit(owner), Ok(1));
    memory.store(key, FUTEX_WAITERS | owner.number);
    assert_eq!(table.unlock(key, owner), Ok(()));
    assert_eq!(memory.word(key), 0);
}

#[test]
fn recursive_trylock_nonowner() {
    let (table, memory, key) = fixture();
    let original = Owner::new(10, 1);
    assert_eq!(
        table.lock(key, original, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    assert_eq!(
        table.lock(key, original, None, false, None, &Interruption::new(), &Clock),
        Err(PiFutexError::Deadlock),
    );
    assert_eq!(
        table.lock(
            key,
            Owner::new(11, 1),
            Some(original),
            true,
            None,
            &Interruption::new(),
            &Clock,
        ),
        Err(PiFutexError::WouldBlock),
    );
    assert_eq!(table.unlock(key, Owner::new(10, 2)), Err(PiFutexError::Permission));
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, 10);
}

#[test]
fn owner_exit_marks() {
    let (table, memory, key) = fixture();
    let first = Owner::new(10, 1);
    let second = Owner::new(11, 1);
    table
        .lock(key, first, None, false, None, &Interruption::new(), &Clock)
        .unwrap();
    let waiter_table = table.clone();
    let waiter =
        thread::spawn(move || waiter_table.lock(key, second, Some(first), false, None, &Interruption::new(), &Clock));
    while memory.word(key) & FUTEX_WAITERS == 0 {
        thread::yield_now();
    }
    assert_eq!(table.owner_exit(first), Ok(1));
    assert_eq!(waiter.join().unwrap(), Ok(PiFutexOutcome::Acquired));
    assert_ne!(memory.word(key) & FUTEX_OWNER_DIED, 0);
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, second.number);
}

#[test]
fn owner_death_hands_off_precise_queue_in_order() {
    let (table, memory, key) = fixture();
    let owner = Owner::new(10, 1);
    let first = Owner::new(11, 1);
    let second = Owner::new(12, 1);
    table
        .lock(key, owner, None, false, None, &Interruption::new(), &Clock)
        .unwrap();

    let first_table = Arc::clone(&table);
    let first_waiter =
        thread::spawn(move || first_table.lock(key, first, Some(owner), false, None, &Interruption::new(), &Clock));
    while table.waiter_observations(key).len() != 1 {
        thread::yield_now();
    }
    let second_table = Arc::clone(&table);
    let second_waiter =
        thread::spawn(move || second_table.lock(key, second, Some(owner), false, None, &Interruption::new(), &Clock));
    while table.waiter_observations(key).len() != 2 {
        thread::yield_now();
    }

    memory.store(key, FUTEX_WAITERS | FUTEX_OWNER_DIED);
    assert_eq!(table.owner_exit(owner), Ok(1));
    assert_eq!(first_waiter.join().unwrap(), Ok(PiFutexOutcome::Acquired));
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, first.number);
    assert_eq!(table.unlock(key, first), Ok(()));
    assert_eq!(second_waiter.join().unwrap(), Ok(PiFutexOutcome::Acquired));
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, second.number);
    assert_eq!(table.unlock(key, second), Ok(()));
}

#[test]
fn userspace_unlock_retires_stale_owner() {
    let (table, memory, key) = fixture();
    let first = Owner::new(10, 1);
    let second = Owner::new(11, 1);
    assert_eq!(
        table.lock(key, first, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    memory.store(key, 0);
    assert_eq!(table.owner_exit(first), Ok(0));
    assert_eq!(memory.word(key), 0);
    assert_eq!(
        table.lock(key, second, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    assert_eq!(memory.word(key), second.number);
    assert_eq!(table.unlock(key, second), Ok(()));
}

#[test]
fn robust_mark_precedes_owner_handoff() {
    let (table, memory, key) = fixture();
    let first = Owner::new(10, 1);
    let second = Owner::new(11, 1);
    assert_eq!(
        table.lock(key, first, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    let waiter_table = table.clone();
    let waiter =
        thread::spawn(move || waiter_table.lock(key, second, Some(first), false, None, &Interruption::new(), &Clock));
    while memory.word(key) & FUTEX_WAITERS == 0 {
        thread::yield_now();
    }
    memory.store(key, FUTEX_WAITERS | FUTEX_OWNER_DIED);
    assert_eq!(table.owner_exit(first), Ok(1));
    assert_eq!(waiter.join().unwrap(), Ok(PiFutexOutcome::Acquired));
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, second.number);
}

#[test]
fn timeout_and_interruption() {
    let (table, memory, key) = fixture();
    let first = Owner::new(10, 1);
    table
        .lock(key, first, None, false, None, &Interruption::new(), &Clock)
        .unwrap();
    assert_eq!(
        table.lock(
            key,
            Owner::new(11, 1),
            Some(first),
            false,
            Some(FutexDeadline {
                clock: FutexClock::Monotonic,
                value: Timespec::ZERO,
            }),
            &Interruption::new(),
            &Clock,
        ),
        Ok(PiFutexOutcome::TimedOut),
    );
    let interruption = Interruption::new();
    interruption.interrupt();
    assert_eq!(
        table.lock(key, Owner::new(12, 1), Some(first), false, None, &interruption, &Clock,),
        Ok(PiFutexOutcome::Interrupted),
    );
    assert_eq!(memory.word(key) & FUTEX_TID_MASK, first.number);

    let interruption = Interruption::new();
    interruption.interrupt();
    assert_eq!(
        table.lock(
            key,
            Owner::new(13, 1),
            Some(first),
            false,
            Some(FutexDeadline {
                clock: FutexClock::Realtime,
                value: Timespec::new(i64::MAX as u64, 999_999_999).unwrap(),
            }),
            &interruption,
            &Clock,
        ),
        Ok(PiFutexOutcome::Interrupted),
    );
}

#[test]
fn capacity_rejection_mutates() {
    let memory = Arc::new(Memory::default());
    let first_key = FutexKey::private(1, 0x1000).unwrap();
    let second_key = FutexKey::private(1, 0x2000).unwrap();
    memory.store(first_key, 0);
    memory.store(second_key, 0);
    let table = PiFutexTable::new(
        memory.clone(),
        |owner: Owner| owner.number,
        crate::FutexLimits {
            buckets: 1,
            keys: 1,
            waiters: 1,
        },
    );
    let first = Owner::new(20, 1);
    assert_eq!(
        table.lock(first_key, first, None, false, None, &Interruption::new(), &Clock),
        Ok(PiFutexOutcome::Acquired),
    );
    assert_eq!(
        table.lock(
            second_key,
            Owner::new(21, 1),
            None,
            false,
            None,
            &Interruption::new(),
            &Clock,
        ),
        Err(PiFutexError::ResourceLimit),
    );
    assert_eq!(memory.word(first_key) & FUTEX_TID_MASK, first.number);
    assert_eq!(memory.word(second_key), 0);
}

#[test]
fn waiter_capacity_rejection() {
    let memory = Arc::new(Memory::default());
    let key = FutexKey::private(1, 0x1000).unwrap();
    memory.store(key, 0);
    let table = PiFutexTable::new(
        memory.clone(),
        |owner: Owner| owner.number,
        crate::FutexLimits {
            buckets: 1,
            keys: 1,
            waiters: 0,
        },
    );
    let first = Owner::new(22, 1);
    table
        .lock(key, first, None, false, None, &Interruption::new(), &Clock)
        .unwrap();
    assert_eq!(
        table.lock(
            key,
            Owner::new(23, 1),
            Some(first),
            false,
            None,
            &Interruption::new(),
            &Clock,
        ),
        Err(PiFutexError::ResourceLimit),
    );
    assert_eq!(memory.word(key), first.number);
    assert_eq!(table.unlock(key, first), Ok(()));
}

#[test]
fn fork_reset_clears() {
    let memory = Arc::new(Memory::default());
    let private = FutexKey::private(1, 0x1000).unwrap();
    let shared = FutexKey::shared(2, 0x2000).unwrap();
    memory.store(private, 0);
    memory.store(shared, 0);
    let table = PiFutexTable::new(
        memory.clone(),
        |owner: Owner| owner.number,
        crate::FutexLimits::default(),
    );
    let owner = Owner::new(30, 1);
    table
        .lock(private, owner, None, false, None, &Interruption::new(), &Clock)
        .unwrap();
    table
        .lock(shared, owner, None, false, None, &Interruption::new(), &Clock)
        .unwrap();
    assert_eq!(table.reset_private(), Ok(1));
    assert_eq!(memory.word(private), 0);
    assert_eq!(memory.word(shared) & FUTEX_TID_MASK, owner.number);
    assert_eq!(table.unlock(shared, owner), Ok(()));
}
