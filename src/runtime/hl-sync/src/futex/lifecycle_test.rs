use std::thread;

use hl_time::Timespec;

use crate::{FutexClock, FutexDeadline, FutexWaitOutcome, Interruption};

use super::test_support::{Clock, fixture};

#[test]
fn expired_realtime_deadline() {
    let (table, _, key, _) = fixture();
    let deadline = FutexDeadline {
        clock: FutexClock::Realtime,
        value: Timespec::from_nanoseconds(99),
    };
    assert_eq!(
        table.wait(
            key,
            3,
            u32::MAX,
            Some(deadline),
            &Interruption::new(),
            &Clock {
                monotonic: 40,
                realtime: 100,
            },
        ),
        Ok(FutexWaitOutcome::TimedOut)
    );
    let interruption = Interruption::new();
    interruption.interrupt();
    assert_eq!(
        table.wait(
            key,
            3,
            u32::MAX,
            None,
            &interruption,
            &Clock {
                monotonic: 0,
                realtime: 0,
            },
        ),
        Ok(FutexWaitOutcome::Interrupted)
    );
}

#[test]
fn far_deadline_saturates() {
    let (table, _, key, _) = fixture();
    for futex_clock in [FutexClock::Monotonic, FutexClock::Realtime] {
        let interruption = Interruption::new();
        interruption.interrupt();
        assert_eq!(
            table.wait(
                key,
                3,
                u32::MAX,
                Some(FutexDeadline {
                    clock: futex_clock,
                    value: Timespec::new(i64::MAX as u64, 999_999_999).unwrap(),
                }),
                &interruption,
                &Clock {
                    monotonic: 40,
                    realtime: 100,
                },
            ),
            Ok(FutexWaitOutcome::Interrupted),
        );
    }
}

#[test]
fn fork_reset_removes() {
    let (table, _, private, shared) = fixture();
    let mut workers = Vec::new();
    for (key, expected) in [(private, 3), (shared, 5)] {
        let table = table.clone();
        workers.push(thread::spawn(move || {
            table.wait(
                key,
                expected,
                u32::MAX,
                None,
                &Interruption::new(),
                &Clock {
                    monotonic: 0,
                    realtime: 0,
                },
            )
        }));
    }
    table.wait_until_registered(2);
    assert_eq!(table.reset_private_process(7), 1);
    assert_eq!(table.snapshot().waits.len(), 1);
    assert_eq!(table.wake(shared, 1, u32::MAX), Ok(1));
    for worker in workers {
        assert_eq!(worker.join().unwrap(), Ok(FutexWaitOutcome::Woken));
    }
}
