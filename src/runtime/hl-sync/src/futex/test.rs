use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use crate::{
    FutexAtomicOperation, FutexError, FutexKey, FutexLimits, FutexMemory, FutexTable, FutexWaitMultipleOutcome,
    FutexWaitOutcome, FutexWaitTarget, Interruption,
};

use super::test_support::{Clock, Memory, fixture};
use super::{FutexBucket, FutexWaiter};
use crate::WaitQueue;

#[test]
#[ignore = "performance diagnostic"]
fn bulk_fifo_wake_benchmark() {
    const WAITERS: usize = 4_096;
    const ROUNDS: usize = 32;
    let key = FutexKey::private(1, 0x1000).unwrap();
    let mut buckets = (0..ROUNDS)
        .map(|_| {
            let mut bucket = FutexBucket::default();
            let queue = bucket.queues.entry(key).or_default();
            for identifier in 0..WAITERS {
                queue.extend(std::iter::once(Arc::new(FutexWaiter {
                    identifier: identifier as u64,
                    bitset: u32::MAX,
                    queue: Arc::new(WaitQueue::new()),
                    vector_index: 0,
                    winner: Arc::new(AtomicUsize::new(usize::MAX)),
                })));
            }
            bucket
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    for bucket in &mut buckets {
        let selected = FutexTable::take_matching(bucket, key, WAITERS, u32::MAX);
        assert_eq!(selected.len(), WAITERS);
        std::hint::black_box(selected);
    }
    let elapsed = started.elapsed().as_nanos() / (ROUNDS * WAITERS) as u128;
    println!("bulk_fifo_wake_ns={elapsed}");
    assert_eq!(buckets.iter().map(|bucket| bucket.queues.len()).sum::<usize>(), 0);
}

#[test]
fn keys_validate_alignment() {
    assert_eq!(FutexKey::private(1, 3), Err(FutexError::InvalidArgument));
    assert_ne!(
        FutexKey::private(1, 0x1000).unwrap(),
        FutexKey::private(2, 0x1000).unwrap()
    );
    assert_eq!(FutexKey::shared(8, 0x40).unwrap(), FutexKey::shared(8, 0x40).unwrap());
}

#[test]
fn compare_and_register() {
    let (table, memory, key, _) = fixture();
    assert_eq!(
        table.wait(
            key,
            4,
            u32::MAX,
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0
            },
        ),
        Err(FutexError::ValueMismatch)
    );
    let worker_table = table.clone();
    let worker = thread::spawn(move || {
        worker_table.wait(
            key,
            3,
            u32::MAX,
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0,
            },
        )
    });
    table.wait_until_registered(1);
    memory.store(key, 4);
    assert_eq!(table.wake(key, 1, u32::MAX), Ok(1));
    assert_eq!(worker.join().unwrap(), Ok(FutexWaitOutcome::Woken));
    assert_eq!(table.wake(key, 1, u32::MAX), Ok(0));
}

#[test]
fn multi_wait_registers() {
    let (table, _, first, second) = fixture();
    let worker_table = table.clone();
    let worker = thread::spawn(move || {
        worker_table.wait_multiple(
            &[
                FutexWaitTarget {
                    key: first,
                    expected: 3,
                },
                FutexWaitTarget {
                    key: second,
                    expected: 5,
                },
            ],
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0,
            },
        )
    });
    table.wait_until_registered(2);
    assert_eq!(table.wake(second, 1, u32::MAX), Ok(1));
    assert_eq!(worker.join().unwrap(), Ok(FutexWaitMultipleOutcome::Woken(1)),);
    assert!(table.snapshot().waits.is_empty());
}

#[test]
fn multi_wait_mismatch() {
    let (table, memory, first, second) = fixture();
    memory.store(second, 6);
    assert_eq!(
        table.wait_multiple(
            &[
                FutexWaitTarget {
                    key: first,
                    expected: 3
                },
                FutexWaitTarget {
                    key: second,
                    expected: 5
                },
            ],
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0
            },
        ),
        Err(FutexError::ValueMismatch),
    );
    let missing = FutexKey::private(7, 0x3000).unwrap();
    assert_eq!(
        table.wait_multiple(
            &[
                FutexWaitTarget {
                    key: first,
                    expected: 3
                },
                FutexWaitTarget {
                    key: missing,
                    expected: 0
                },
            ],
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0
            },
        ),
        Err(FutexError::Fault),
    );
    assert!(table.snapshot().waits.is_empty());
}

#[test]
fn duplicate_multi_wait() {
    let (table, _, key, _) = fixture();
    let worker_table = table.clone();
    let worker = thread::spawn(move || {
        worker_table.wait_multiple(
            &[
                FutexWaitTarget { key, expected: 3 },
                FutexWaitTarget { key, expected: 3 },
            ],
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0,
            },
        )
    });
    table.wait_until_registered(2);
    assert_eq!(table.wake(key, 1, u32::MAX), Ok(1));
    assert_eq!(worker.join().unwrap(), Ok(FutexWaitMultipleOutcome::Woken(0)),);
    assert_eq!(table.wake(key, 1, u32::MAX), Ok(0));
}

#[test]
fn multi_wait_key() {
    let memory = Arc::new(Memory::default());
    let first = FutexKey::private(1, 0x1000).unwrap();
    let second = FutexKey::private(1, 0x2000).unwrap();
    memory.store(first, 1);
    memory.store(second, 2);
    let table = FutexTable::new(
        FutexLimits {
            buckets: 1,
            keys: 1,
            waiters: 4,
        },
        memory,
    )
    .unwrap();
    assert_eq!(
        table.wait_multiple(
            &[
                FutexWaitTarget {
                    key: first,
                    expected: 1
                },
                FutexWaitTarget {
                    key: second,
                    expected: 2
                },
            ],
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0
            },
        ),
        Err(FutexError::ResourceLimit),
    );
    assert!(table.snapshot().waits.is_empty());
}

#[test]
fn multi_wait_wake() {
    for _ in 0..32 {
        let (table, _, key, _) = fixture();
        let interruption = Arc::new(Interruption::new());
        let worker_table = table.clone();
        let worker_interruption = interruption.clone();
        let worker = thread::spawn(move || {
            worker_table.wait_multiple(
                &[FutexWaitTarget { key, expected: 3 }],
                None,
                &worker_interruption,
                &Clock {
                    monotonic: 0,
                    realtime: 0,
                },
            )
        });
        table.wait_until_registered(1);
        let wake_table = table.clone();
        let wake = thread::spawn(move || wake_table.wake(key, 1, u32::MAX));
        interruption.interrupt();
        let wake = wake.join().unwrap().unwrap();
        let outcome = worker.join().unwrap().unwrap();
        if wake == 1 {
            assert_eq!(outcome, FutexWaitMultipleOutcome::Woken(0));
        } else {
            assert_eq!(outcome, FutexWaitMultipleOutcome::Interrupted);
        }
    }
}

#[test]
fn bitset_wake_selects() {
    let (table, _, key, _) = fixture();
    let mut workers = Vec::new();
    for bitset in [1, 2] {
        let table = table.clone();
        workers.push((
            bitset,
            thread::spawn(move || {
                table.wait(
                    key,
                    3,
                    bitset,
                    None,
                    &Interruption::new(),
                    &Clock {
                        monotonic: 0,
                        realtime: 0,
                    },
                )
            }),
        ));
    }
    table.wait_until_registered(2);
    assert_eq!(table.wake(key, 1, 1), Ok(1));
    assert_eq!(table.snapshot().waits[0].bitset, 2);
    assert_eq!(table.wake(key, 1, 2), Ok(1));
    for (_, worker) in workers {
        assert_eq!(worker.join().unwrap(), Ok(FutexWaitOutcome::Woken));
    }
}

#[test]
fn cmp_requeue_moves() {
    let (table, _, source, target) = fixture();
    let worker_table = table.clone();
    let worker = thread::spawn(move || {
        worker_table.wait(
            source,
            3,
            u32::MAX,
            None,
            &Interruption::new(),
            &Clock {
                monotonic: 0,
                realtime: 0,
            },
        )
    });
    table.wait_until_registered(1);
    assert_eq!(table.requeue(source, target, 0, 1, Some(3)), Ok(0));
    assert_eq!(table.snapshot().waits[0].key, target);
    assert_eq!(table.wake(target, 1, u32::MAX), Ok(1));
    assert_eq!(worker.join().unwrap(), Ok(FutexWaitOutcome::Woken));
}

#[test]
fn cmp_requeue_and() {
    for _ in 0..64 {
        let (table, memory, source, target) = fixture();
        let worker_table = table.clone();
        let waiter = thread::spawn(move || {
            worker_table.wait(
                source,
                3,
                u32::MAX,
                None,
                &Interruption::new(),
                &Clock {
                    monotonic: 0,
                    realtime: 0,
                },
            )
        });
        table.wait_until_registered(1);

        let (sender, receiver) = mpsc::channel();
        let requeue_table = table.clone();
        let requeue_sender = sender.clone();
        thread::spawn(move || {
            let result = requeue_table.requeue(source, target, 0, 1, Some(3));
            requeue_sender.send(result).unwrap();
        });
        let store_sender = sender;
        thread::spawn(move || {
            memory.store(source, 4);
            store_sender.send(Ok(usize::MAX)).unwrap();
        });

        let first = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let requeue = if first == Ok(usize::MAX) { second } else { first };
        assert!(matches!(requeue, Ok(0) | Err(FutexError::CompareMismatch)));

        let queued_key = table.snapshot().waits[0].key;
        assert_eq!(queued_key, if requeue == Ok(0) { target } else { source });
        assert_eq!(table.wake(queued_key, 1, u32::MAX), Ok(1));
        assert_eq!(waiter.join().unwrap(), Ok(FutexWaitOutcome::Woken));
    }
}

#[test]
fn wake_op_mutates() {
    let (table, memory, first, second) = fixture();
    assert_eq!(
        table.wake_op(first, second, 1, 1, FutexAtomicOperation::Add(2), |old| old == 5,),
        Ok(0)
    );
    assert_eq!(memory.load(second), Ok(7));
}
