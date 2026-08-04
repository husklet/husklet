use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use hl_sync::Interruption;
use hl_time::{ClockError, Deadline, MonotonicClock, MonotonicInstant};

use crate::{
    Credentials, IPC_NOWAIT, IpcKey, SEM_UNDO, SemGetRequest, SemaphoreError, SemaphoreLimits, SemaphoreNamespace,
    SemaphoreOperation,
};

const OWNER: Credentials = Credentials { uid: 10, gid: 20 };
const OTHER: Credentials = Credentials { uid: 11, gid: 21 };

struct Fixture;

impl Fixture {
    fn namespace(limits: SemaphoreLimits) -> SemaphoreNamespace {
        SemaphoreNamespace::new(limits).unwrap()
    }

    fn request(key: IpcKey, semaphores: usize) -> SemGetRequest {
        SemGetRequest {
            key,
            semaphores,
            create: true,
            exclusive: false,
            mode: 0o600,
            actor: OWNER,
            pid: 100,
            now: 1,
        }
    }

    fn operation(index: u16, delta: i32) -> SemaphoreOperation {
        SemaphoreOperation { index, delta, flags: 0 }
    }
}

#[derive(Debug)]
struct Clock(AtomicU64);

impl MonotonicClock for Clock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(self.0.load(Ordering::Acquire)))
    }
}

#[test]
fn keys_exclusive_permissions() {
    let namespace = Fixture::namespace(SemaphoreLimits::default());
    let id = namespace.semget(Fixture::request(IpcKey(1), 2)).unwrap();
    assert_eq!(namespace.semget(Fixture::request(IpcKey(1), 1)), Ok(id));
    let mut exclusive = Fixture::request(IpcKey(1), 2);
    exclusive.exclusive = true;
    assert_eq!(namespace.semget(exclusive), Err(SemaphoreError::Exists));
    assert_eq!(namespace.get_value(id, 0, OTHER), Err(SemaphoreError::Permission));
    namespace.set_permissions(id, OWNER, OTHER, 0o600, 2).unwrap();
    assert_eq!(namespace.get_value(id, 0, OTHER), Ok(0));
    assert_eq!(namespace.get_value(id, 0, OWNER), Ok(0));
    namespace.remove(id, OWNER, 100, 3).unwrap();
    assert_eq!(namespace.get_value(id, 0, OTHER), Err(SemaphoreError::Removed));
    let next = namespace.semget(Fixture::request(IpcKey(1), 2)).unwrap();
    assert_eq!(next.slot, id.slot);
    assert_ne!(next.generation, id.generation);
}

#[test]
fn operation_vectors_are() {
    let limits = SemaphoreLimits {
        maximum_value: 5,
        operations: 2,
        ..SemaphoreLimits::default()
    };
    let namespace = Fixture::namespace(limits);
    let id = namespace.semget(Fixture::request(IpcKey(2), 2)).unwrap();
    namespace.set_all(id, &[2, 0], OWNER, 1, 2).unwrap();
    assert_eq!(
        namespace.operate(id, OWNER, 1, &[Fixture::operation(0, -1), Fixture::operation(1, -1)], 3,),
        Err(SemaphoreError::Again)
    );
    assert_eq!(namespace.get_all(id, OWNER).unwrap(), [2, 0]);
    assert_eq!(
        namespace.operate(id, OWNER, 1, &[Fixture::operation(0, 4)], 3),
        Err(SemaphoreError::Range)
    );
    assert_eq!(
        namespace.operate(
            id,
            OWNER,
            1,
            &[
                Fixture::operation(0, 0),
                Fixture::operation(0, 0),
                Fixture::operation(0, 0)
            ],
            3,
        ),
        Err(SemaphoreError::InvalidArgument)
    );
}

#[test]
fn undo_lifecycle() {
    let namespace = Fixture::namespace(SemaphoreLimits::default());
    let id = namespace.semget(Fixture::request(IpcKey(3), 1)).unwrap();
    namespace.set_value(id, 0, 3, OWNER, 1, 2).unwrap();
    let operation = SemaphoreOperation {
        index: 0,
        delta: -2,
        flags: SEM_UNDO,
    };
    namespace.operate(id, OWNER, 10, &[operation], 3).unwrap();
    assert_eq!(namespace.get_value(id, 0, OWNER), Ok(1));
    namespace.exec(10);
    namespace.fork(10, 20);
    namespace.exit(20, 4);
    assert_eq!(namespace.get_value(id, 0, OWNER), Ok(1));
    namespace.exit(10, 5);
    assert_eq!(namespace.get_value(id, 0, OWNER), Ok(3));
    assert_eq!(namespace.get_pid(id, 0, OWNER), Ok(10));
}

#[test]
fn prepared_child_undo() {
    let namespace = Arc::new(Fixture::namespace(SemaphoreLimits::default()));
    let id = namespace.semget(Fixture::request(IpcKey(30), 1)).unwrap();
    namespace.set_value(id, 0, 5, OWNER, 1, 2).unwrap();
    let operation = SemaphoreOperation {
        index: 0,
        delta: -2,
        flags: SEM_UNDO,
    };
    namespace.operate(id, OWNER, 20, &[operation], 3).unwrap();
    let before = namespace.snapshot();
    let prepared = namespace.prepare_fork_child(20);
    assert_eq!(namespace.snapshot(), before);
    let committed = prepared.commit().unwrap();
    assert!(
        namespace
            .snapshot()
            .undo
            .iter()
            .all(|(process, _, _, _)| *process != 20)
    );
    committed.rollback().unwrap();
    assert_eq!(namespace.snapshot(), before);
}

#[test]
fn prepared_child_state() {
    let namespace = Arc::new(Fixture::namespace(SemaphoreLimits::default()));
    let id = namespace.semget(Fixture::request(IpcKey(31), 1)).unwrap();
    namespace.set_value(id, 0, 5, OWNER, 1, 2).unwrap();
    namespace
        .operate(
            id,
            OWNER,
            20,
            &[SemaphoreOperation {
                index: 0,
                delta: -1,
                flags: SEM_UNDO,
            }],
            3,
        )
        .unwrap();
    let prepared = namespace.prepare_fork_child(20);
    namespace.remove(id, OWNER, 20, 4).unwrap();
    let before = namespace.snapshot();
    assert_eq!(prepared.commit().map(|_| ()), Err(SemaphoreError::InvalidArgument),);
    assert_eq!(namespace.snapshot(), before);
}

#[test]
fn wait_counts_interruption() {
    let namespace = Arc::new(Fixture::namespace(SemaphoreLimits::default()));
    let id = namespace.semget(Fixture::request(IpcKey(4), 1)).unwrap();
    let worker_namespace = namespace.clone();
    let worker = thread::spawn(move || {
        worker_namespace.operate_wait(
            id,
            OWNER,
            1,
            &[Fixture::operation(0, -1)],
            &Interruption::new(),
            None,
            &Clock(AtomicU64::new(0)),
            2,
        )
    });
    while namespace.get_wait_counts(id, 0, OWNER).unwrap().0 == 0 {
        thread::yield_now();
    }
    namespace.remove(id, OWNER, 1, 3).unwrap();
    assert_eq!(worker.join().unwrap(), Err(SemaphoreError::Removed));

    let id = namespace.semget(Fixture::request(IpcKey(4), 1)).unwrap();
    let interruption = Interruption::new();
    interruption.interrupt();
    assert_eq!(
        namespace.operate_wait(
            id,
            OWNER,
            1,
            &[Fixture::operation(0, -1)],
            &interruption,
            None,
            &Clock(AtomicU64::new(0)),
            4,
        ),
        Err(SemaphoreError::Interrupted)
    );
    assert_eq!(
        namespace.operate_wait(
            id,
            OWNER,
            1,
            &[Fixture::operation(0, -1)],
            &Interruption::new(),
            Some(Deadline::from_nanoseconds(0)),
            &Clock(AtomicU64::new(0)),
            4,
        ),
        Err(SemaphoreError::TimedOut)
    );
    assert_eq!(namespace.get_wait_counts(id, 0, OWNER), Ok((0, 0)));
}

#[test]
fn nowait_zero_and() {
    let namespace = Fixture::namespace(SemaphoreLimits::default());
    let id = namespace.semget(Fixture::request(IpcKey(5), 1)).unwrap();
    namespace.set_value(id, 0, 1, OWNER, 1, 2).unwrap();
    for delta in [0, -2] {
        let operation = SemaphoreOperation {
            index: 0,
            delta,
            flags: IPC_NOWAIT,
        };
        assert_eq!(
            namespace.operate_wait(
                id,
                OWNER,
                1,
                &[operation],
                &Interruption::new(),
                None,
                &Clock(AtomicU64::new(0)),
                3,
            ),
            Err(SemaphoreError::Again)
        );
    }
    assert_eq!(namespace.get_wait_counts(id, 0, OWNER), Ok((0, 0)));
}

#[test]
fn snapshot_restore_roundtrips() {
    let namespace = Fixture::namespace(SemaphoreLimits::default());
    let id = namespace.semget(Fixture::request(IpcKey(6), 2)).unwrap();
    namespace.set_all(id, &[2, 4], OWNER, 1, 2).unwrap();
    namespace
        .operate(
            id,
            OWNER,
            10,
            &[SemaphoreOperation {
                index: 1,
                delta: -1,
                flags: SEM_UNDO,
            }],
            3,
        )
        .unwrap();
    let snapshot = namespace.snapshot();
    let restored = SemaphoreNamespace::restore(SemaphoreLimits::default(), snapshot.clone()).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
    assert_eq!(restored.get_all(id, OWNER).unwrap(), [2, 3]);
}

#[test]
fn concurrent_atomic_increments() {
    let limits = SemaphoreLimits {
        maximum_value: 64,
        ..SemaphoreLimits::default()
    };
    let namespace = Arc::new(Fixture::namespace(limits));
    let id = namespace.semget(Fixture::request(IpcKey(7), 1)).unwrap();
    let workers: Vec<_> = (0..128)
        .map(|pid| {
            let namespace = namespace.clone();
            thread::spawn(move || namespace.operate(id, OWNER, pid, &[Fixture::operation(0, 1)], 2))
        })
        .collect();
    let successes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .filter(Result::is_ok)
        .count();
    assert_eq!(successes, 64);
    assert_eq!(namespace.get_value(id, 0, OWNER), Ok(64));
}
