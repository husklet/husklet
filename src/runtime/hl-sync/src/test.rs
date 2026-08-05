use super::{Interruption, WaitOutcome, WaitQueue};
use hl_time::{ClockError, Deadline, MonotonicClock, MonotonicInstant};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

struct FixedClock(AtomicU64);

impl FixedClock {
    fn set(&self, value: u64) {
        self.0.store(value, Ordering::Release);
    }
}

impl MonotonicClock for FixedClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(self.0.load(Ordering::Acquire)))
    }
}

struct ElapsedClock(Instant);

impl MonotonicClock for ElapsedClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        let elapsed = self.0.elapsed().as_nanos();
        Ok(MonotonicInstant::from_nanoseconds(
            u64::try_from(elapsed).unwrap_or(u64::MAX),
        ))
    }
}

#[test]
fn notification_between_observation() {
    let queue = WaitQueue::new();
    let observed = queue.observation();
    assert_eq!(queue.notify_one(), 0);
    assert_eq!(
        queue
            .wait(observed, &Interruption::new(), None, &FixedClock(AtomicU64::new(0)))
            .unwrap(),
        WaitOutcome::Notified
    );
}

#[test]
fn pending_interruption_is() {
    let queue = WaitQueue::new();
    let interruption = Interruption::new();
    let clock = FixedClock(AtomicU64::new(10));
    interruption.interrupt();
    interruption.interrupt();

    assert_eq!(
        queue.wait(queue.observation(), &interruption, None, &clock).unwrap(),
        WaitOutcome::Interrupted
    );
    assert!(!interruption.is_pending());
    assert_eq!(
        queue
            .wait(
                queue.observation(),
                &interruption,
                Some(Deadline::from_nanoseconds(10)),
                &clock
            )
            .unwrap(),
        WaitOutcome::TimedOut
    );
}

#[test]
fn interruption_wakes_a() {
    let queue = Arc::new(WaitQueue::new());
    let interruption = Interruption::new();
    let ready = Arc::new(Barrier::new(2));
    let worker_queue = Arc::clone(&queue);
    let worker_interruption = interruption.clone();
    let worker_ready = Arc::clone(&ready);
    let worker = thread::spawn(move || {
        let observed = worker_queue.observation();
        worker_ready.wait();
        worker_queue
            .wait(observed, &worker_interruption, None, &FixedClock(AtomicU64::new(0)))
            .unwrap()
    });
    ready.wait();
    while !interruption
        .state
        .waiters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|(_, waiter)| waiter.strong_count() != 0)
    {
        thread::yield_now();
    }
    interruption.interrupt();
    assert_eq!(worker.join().unwrap(), WaitOutcome::Interrupted);
}

#[test]
fn notify_one_releases() {
    let queue = Arc::new(WaitQueue::new());
    let started = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let queue = Arc::clone(&queue);
        let started = Arc::clone(&started);
        workers.push(thread::spawn(move || {
            let observed = queue.observation();
            started.fetch_add(1, Ordering::Release);
            queue
                .wait(
                    observed,
                    &Interruption::new(),
                    Some(Deadline::from_nanoseconds(50_000_000)),
                    &ElapsedClock(Instant::now()),
                )
                .unwrap()
        }));
    }
    while started.load(Ordering::Acquire) != 2 {
        thread::yield_now();
    }
    while queue
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .waiters
        .iter()
        .filter(|waiter| waiter.strong_count() != 0)
        .count()
        != 2
    {
        thread::yield_now();
    }
    assert_eq!(queue.notify_one(), 1);
    let outcomes: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == WaitOutcome::Notified)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == WaitOutcome::TimedOut)
            .count(),
        1
    );
}

#[test]
fn notify_all_releases() {
    let queue = Arc::new(WaitQueue::new());
    let ready = Arc::new(Barrier::new(4));
    let mut workers = Vec::new();
    for _ in 0..3 {
        let queue = Arc::clone(&queue);
        let ready = Arc::clone(&ready);
        workers.push(thread::spawn(move || {
            let observed = queue.observation();
            ready.wait();
            queue
                .wait(observed, &Interruption::new(), None, &FixedClock(AtomicU64::new(0)))
                .unwrap()
        }));
    }
    ready.wait();
    loop {
        let count = queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiters
            .iter()
            .filter(|waiter| waiter.strong_count() != 0)
            .count();
        if count == 3 {
            break;
        }
        thread::yield_now();
    }
    assert_eq!(queue.notify_all(), 3);
    for worker in workers {
        assert_eq!(worker.join().unwrap(), WaitOutcome::Notified);
    }
}

#[test]
fn expired_deadline_never() {
    let queue = WaitQueue::new();
    let clock = FixedClock(AtomicU64::new(101));
    assert_eq!(
        queue
            .wait(
                queue.observation(),
                &Interruption::new(),
                Some(Deadline::from_nanoseconds(100)),
                &clock
            )
            .unwrap(),
        WaitOutcome::TimedOut
    );
}

#[test]
fn deadline_uses_absolute() {
    let queue = WaitQueue::new();
    let clock = ElapsedClock(Instant::now());
    let start = Instant::now();
    assert_eq!(
        queue
            .wait(
                queue.observation(),
                &Interruption::new(),
                Some(Deadline::from_nanoseconds(10_000_000)),
                &clock
            )
            .unwrap(),
        WaitOutcome::TimedOut
    );
    assert!(start.elapsed() >= StdDuration::from_millis(5));
}

#[test]
fn manual_clock_deadline() {
    let queue = Arc::new(WaitQueue::new());
    let clock = Arc::new(FixedClock(AtomicU64::new(10)));
    let interruption = Interruption::new();
    let worker_queue = Arc::clone(&queue);
    let worker_clock = Arc::clone(&clock);
    let worker = thread::spawn(move || {
        worker_queue
            .wait(
                worker_queue.observation(),
                &interruption,
                Some(Deadline::from_nanoseconds(20)),
                worker_clock.as_ref(),
            )
            .unwrap()
    });
    while queue
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .waiters
        .is_empty()
    {
        thread::yield_now();
    }
    clock.set(20);
    // A spurious wake must re-read the injected clock and observe expiry.
    let waiter = queue
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .waiters[0]
        .upgrade()
        .unwrap();
    waiter.signal();
    assert_eq!(worker.join().unwrap(), WaitOutcome::TimedOut);
}
