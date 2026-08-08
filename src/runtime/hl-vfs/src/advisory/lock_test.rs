use std::sync::Arc;
use std::thread;

use crate::{
    AdvisoryLockCoordinator, FlockMode, FlockOwnerToken, Identity, LockCancellation, LockError, LockRange,
    ProcessLockOwner, RangeLockKind, RangeLockRequest, RangeWhence,
};

struct LockFixture;

impl LockFixture {
    fn file(inode: u64) -> Identity {
        Identity { device: 1, inode }
    }

    fn process(identity: u64) -> ProcessLockOwner {
        ProcessLockOwner {
            identity,
            generation: 1,
        }
    }

    fn ofd(identity: u64) -> FlockOwnerToken {
        FlockOwnerToken {
            identity,
            generation: 1,
        }
    }

    fn range(start: u64, end: Option<u64>) -> LockRange {
        LockRange { start, end }
    }
}

#[test]
fn signed_ranges_eof() {
    assert_eq!(
        LockRange::normalize(
            RangeLockRequest {
                kind: RangeLockKind::Write,
                whence: RangeWhence::Current,
                start: -10,
                length: 5,
            },
            20,
            100,
        ),
        Ok(LockFixture::range(10, Some(15)))
    );
    assert_eq!(
        LockRange::normalize(
            RangeLockRequest {
                kind: RangeLockKind::Read,
                whence: RangeWhence::End,
                start: 0,
                length: -20,
            },
            0,
            100,
        ),
        Ok(LockFixture::range(80, Some(100)))
    );
    assert_eq!(
        LockRange::normalize(
            RangeLockRequest {
                kind: RangeLockKind::Read,
                whence: RangeWhence::Start,
                start: 4,
                length: 0,
            },
            0,
            0,
        ),
        Ok(LockFixture::range(4, None))
    );
}

#[test]
fn final_ofd_close_releases_range_locks() {
    let locks = AdvisoryLockCoordinator::new();
    let file = LockFixture::file(71);
    let ofd = LockFixture::ofd(9);
    let owner = ProcessLockOwner::open_file(ofd);
    let other = LockFixture::process(12);
    let range = LockFixture::range(0, Some(16));
    let cancellation = LockCancellation::default();
    locks
        .set_range(
            file,
            owner,
            Some(RangeLockKind::Write),
            range,
            false,
            false,
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        locks.set_range(
            file,
            other,
            Some(RangeLockKind::Write),
            range,
            false,
            false,
            &cancellation
        ),
        Err(LockError::WouldBlock),
    );

    locks.close_ofd(ofd);
    locks
        .set_range(
            file,
            other,
            Some(RangeLockKind::Write),
            range,
            false,
            false,
            &cancellation,
        )
        .unwrap();
}

/// Copy-up gives one guest file two host identities. A lock held through the
/// lower must move to the upper, and a descriptor that still stats the lower
/// must keep landing there afterwards.
#[test]
fn copy_up_keeps_lower_and_upper_on_one_lock_identity() {
    let locks = AdvisoryLockCoordinator::new();
    let lower = LockFixture::file(41);
    let upper = LockFixture::file(42);
    let holder = LockFixture::process(1);
    let writer = LockFixture::process(2);
    let cancellation = LockCancellation::default();
    let held = LockFixture::range(0, Some(100));
    let later = LockFixture::range(200, Some(300));
    locks
        .set_range(
            lower,
            holder,
            Some(RangeLockKind::Read),
            held,
            false,
            false,
            &cancellation,
        )
        .unwrap();

    locks.unify(lower, upper);
    assert_eq!(
        locks.set_range(
            upper,
            writer,
            Some(RangeLockKind::Write),
            held,
            false,
            false,
            &cancellation
        ),
        Err(LockError::WouldBlock),
    );

    locks
        .set_range(
            lower,
            holder,
            Some(RangeLockKind::Read),
            later,
            false,
            false,
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        locks.set_range(
            upper,
            writer,
            Some(RangeLockKind::Write),
            later,
            false,
            false,
            &cancellation
        ),
        Err(LockError::WouldBlock),
    );
}

/// An upper whose last link is going away must stop attracting the lower, or a
/// recycled inode number inherits the translation.
#[test]
fn forgetting_an_upper_drops_its_translation() {
    let locks = AdvisoryLockCoordinator::new();
    let lower = LockFixture::file(51);
    let upper = LockFixture::file(52);
    let cancellation = LockCancellation::default();
    let range = LockFixture::range(0, Some(100));
    locks.unify(lower, upper);
    locks.forget(upper);
    locks
        .set_range(
            lower,
            LockFixture::process(1),
            Some(RangeLockKind::Write),
            range,
            false,
            false,
            &cancellation,
        )
        .unwrap();
    locks
        .set_range(
            upper,
            LockFixture::process(2),
            Some(RangeLockKind::Write),
            range,
            false,
            false,
            &cancellation,
        )
        .unwrap();
}

#[test]
fn range_conflicts_coalesce() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    let cancel = LockCancellation::default();
    locks
        .set_range(
            LockFixture::file(1),
            LockFixture::process(1),
            Some(RangeLockKind::Write),
            LockFixture::range(0, Some(100)),
            false,
            false,
            &cancel,
        )
        .unwrap();
    assert_eq!(
        locks.set_range(
            LockFixture::file(1),
            LockFixture::process(2),
            Some(RangeLockKind::Read),
            LockFixture::range(50, Some(60)),
            false,
            false,
            &cancel,
        ),
        Err(LockError::WouldBlock)
    );
    assert_eq!(
        locks
            .query_range(
                LockFixture::file(1),
                LockFixture::process(2),
                RangeLockKind::Write,
                LockFixture::range(10, Some(20)),
                false,
            )
            .unwrap()
            .owner,
        LockFixture::process(1)
    );
    locks
        .set_range(
            LockFixture::file(1),
            LockFixture::process(1),
            None,
            LockFixture::range(25, Some(75)),
            false,
            false,
            &cancel,
        )
        .unwrap();
    let snapshot = locks.snapshot();
    assert_eq!(snapshot.ranges.len(), 2);
    assert_eq!(snapshot.ranges[0].range, LockFixture::range(0, Some(25)));
    assert_eq!(snapshot.ranges[1].range, LockFixture::range(75, Some(100)));
    locks
        .set_range(
            LockFixture::file(1),
            LockFixture::process(1),
            Some(RangeLockKind::Write),
            LockFixture::range(25, Some(75)),
            false,
            false,
            &cancel,
        )
        .unwrap();
    assert_eq!(locks.snapshot().ranges[0].range, LockFixture::range(0, Some(100)));
}

#[test]
fn flock_upgrade_ownership() {
    let locks = AdvisoryLockCoordinator::new();
    let cancel = LockCancellation::default();
    locks
        .set_flock(
            LockFixture::file(1),
            LockFixture::ofd(1),
            Some(FlockMode::Shared),
            false,
            &cancel,
        )
        .unwrap();
    locks
        .set_flock(
            LockFixture::file(1),
            LockFixture::ofd(2),
            Some(FlockMode::Shared),
            false,
            &cancel,
        )
        .unwrap();
    assert_eq!(
        locks.set_flock(
            LockFixture::file(1),
            LockFixture::ofd(1),
            Some(FlockMode::Exclusive),
            false,
            &cancel
        ),
        Err(LockError::WouldBlock)
    );
    locks.close_ofd(LockFixture::ofd(2));
    locks
        .set_flock(
            LockFixture::file(1),
            LockFixture::ofd(1),
            Some(FlockMode::Exclusive),
            false,
            &cancel,
        )
        .unwrap();
    locks
        .set_flock(
            LockFixture::file(1),
            LockFixture::ofd(1),
            Some(FlockMode::Shared),
            false,
            &cancel,
        )
        .unwrap();
    assert_eq!(locks.snapshot().flocks.len(), 1);
    locks.close_ofd(LockFixture::ofd(1));
    assert!(locks.snapshot().flocks.is_empty());
}

#[test]
fn posix_fd_locks() {
    let locks = AdvisoryLockCoordinator::new();
    let cancel = LockCancellation::default();
    for owner in [LockFixture::process(1), LockFixture::process(2)] {
        locks
            .set_range(
                LockFixture::file(1),
                owner,
                Some(RangeLockKind::Read),
                LockFixture::range(0, Some(10)),
                false,
                false,
                &cancel,
            )
            .unwrap();
    }
    locks
        .close_process_file(LockFixture::file(1), LockFixture::process(1), false)
        .unwrap();
    let snapshot = locks.snapshot();
    assert_eq!(snapshot.ranges.len(), 1);
    assert_eq!(snapshot.ranges[0].owner, LockFixture::process(2));
}

#[test]
fn exit_rollback_isolated() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    let cancel = LockCancellation::default();
    let exiting = LockFixture::process(1);
    let other = LockFixture::process(2);
    for (file, owner, start) in [
        (LockFixture::file(1), exiting, 0),
        (LockFixture::file(2), exiting, 10),
        (LockFixture::file(3), other, 20),
    ] {
        locks
            .set_range(
                file,
                owner,
                Some(RangeLockKind::Read),
                LockFixture::range(start, Some(start + 5)),
                false,
                false,
                &cancel,
            )
            .unwrap();
    }
    let mut prepared = locks.prepare_exit(exiting).unwrap();
    locks
        .set_range(
            LockFixture::file(4),
            other,
            Some(RangeLockKind::Write),
            LockFixture::range(30, Some(40)),
            false,
            false,
            &cancel,
        )
        .unwrap();
    prepared.publish().unwrap();
    assert!(locks.snapshot().ranges.iter().all(|record| record.owner != exiting));
    locks
        .set_range(
            LockFixture::file(5),
            other,
            Some(RangeLockKind::Read),
            LockFixture::range(50, Some(60)),
            false,
            false,
            &cancel,
        )
        .unwrap();
    prepared.rollback();
    let snapshot = locks.snapshot();
    assert_eq!(
        snapshot.ranges.iter().filter(|record| record.owner == exiting).count(),
        2
    );
    assert_eq!(snapshot.ranges.iter().filter(|record| record.owner == other).count(), 3);
}

#[test]
fn exit_admission_rejects() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    let cancel = LockCancellation::default();
    let owner = LockFixture::process(1);
    locks
        .set_range(
            LockFixture::file(1),
            owner,
            Some(RangeLockKind::Read),
            LockFixture::range(0, Some(10)),
            false,
            false,
            &cancel,
        )
        .unwrap();
    let mut prepared = locks.prepare_exit(owner).unwrap();
    assert_eq!(
        locks.set_range(
            LockFixture::file(2),
            owner,
            Some(RangeLockKind::Read),
            LockFixture::range(0, Some(10)),
            false,
            false,
            &cancel,
        ),
        Err(LockError::ConcurrentMutation),
    );
    prepared.publish().unwrap();
    prepared.rollback();
    assert_eq!(locks.snapshot().ranges.len(), 1);
}

#[test]
fn exit_admission_holds() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    let cancel = LockCancellation::default();
    let owner = LockFixture::process(1);
    locks
        .set_range(
            LockFixture::file(1),
            owner,
            Some(RangeLockKind::Read),
            LockFixture::range(0, Some(10)),
            false,
            false,
            &cancel,
        )
        .unwrap();
    let mut prepared = locks.prepare_exit(owner).unwrap();
    prepared.publish().unwrap();
    assert_eq!(
        locks.set_range(
            LockFixture::file(2),
            owner,
            Some(RangeLockKind::Write),
            LockFixture::range(20, Some(30)),
            false,
            false,
            &cancel,
        ),
        Err(LockError::ConcurrentMutation),
    );
    assert_eq!(
        locks.close_process_file(LockFixture::file(1), owner, false),
        Err(LockError::ConcurrentMutation),
    );
    prepared.rollback();
    let snapshot = locks.snapshot();
    assert_eq!(snapshot.ranges.len(), 1);
    assert_eq!(snapshot.ranges[0].file, LockFixture::file(1));
}

#[test]
fn blocking_requests_writer() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    let held = Arc::new(LockCancellation::default());
    locks
        .set_flock(
            LockFixture::file(1),
            LockFixture::ofd(1),
            Some(FlockMode::Exclusive),
            false,
            &held,
        )
        .unwrap();
    let writer_locks = locks.clone();
    let writer = thread::spawn(move || {
        writer_locks
            .set_flock(
                LockFixture::file(1),
                LockFixture::ofd(2),
                Some(FlockMode::Exclusive),
                true,
                &LockCancellation::default(),
            )
            .unwrap();
    });
    while locks.waiting() != 1 {
        thread::yield_now();
    }
    let reader_locks = locks.clone();
    let reader = thread::spawn(move || {
        reader_locks
            .set_flock(
                LockFixture::file(1),
                LockFixture::ofd(3),
                Some(FlockMode::Shared),
                true,
                &LockCancellation::default(),
            )
            .unwrap();
    });
    while locks.waiting() != 2 {
        thread::yield_now();
    }
    locks.close_ofd(LockFixture::ofd(1));
    writer.join().unwrap();
    assert_eq!(locks.snapshot().flocks[0].owner, LockFixture::ofd(2));
    locks.close_ofd(LockFixture::ofd(2));
    reader.join().unwrap();
    assert_eq!(locks.snapshot().flocks[0].owner, LockFixture::ofd(3));
}

#[test]
fn two_file_deadlock() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    let cancel = LockCancellation::default();
    locks
        .set_range(
            LockFixture::file(1),
            LockFixture::process(1),
            Some(RangeLockKind::Write),
            LockFixture::range(0, None),
            false,
            false,
            &cancel,
        )
        .unwrap();
    locks
        .set_range(
            LockFixture::file(2),
            LockFixture::process(2),
            Some(RangeLockKind::Write),
            LockFixture::range(0, None),
            false,
            false,
            &cancel,
        )
        .unwrap();
    let waiting_locks = locks.clone();
    let waiter = thread::spawn(move || {
        waiting_locks.set_range(
            LockFixture::file(2),
            LockFixture::process(1),
            Some(RangeLockKind::Write),
            LockFixture::range(0, None),
            true,
            false,
            &LockCancellation::default(),
        )
    });
    while locks.waiting() != 1 {
        thread::yield_now();
    }
    assert_eq!(
        locks.set_range(
            LockFixture::file(1),
            LockFixture::process(2),
            Some(RangeLockKind::Write),
            LockFixture::range(0, None),
            true,
            false,
            &cancel,
        ),
        Err(LockError::Deadlock)
    );
    locks
        .close_process_file(LockFixture::file(2), LockFixture::process(2), false)
        .unwrap();
    assert_eq!(waiter.join().unwrap(), Ok(()));
}

#[test]
fn interruption_removes_lock() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    locks
        .set_flock(
            LockFixture::file(1),
            LockFixture::ofd(1),
            Some(FlockMode::Exclusive),
            false,
            &LockCancellation::default(),
        )
        .unwrap();
    let cancellation = Arc::new(LockCancellation::default());
    let waiter_locks = locks.clone();
    let waiter_cancel = cancellation.clone();
    let waiter = thread::spawn(move || {
        waiter_locks.set_flock(
            LockFixture::file(1),
            LockFixture::ofd(2),
            Some(FlockMode::Exclusive),
            true,
            &waiter_cancel,
        )
    });
    while locks.waiting() != 1 {
        thread::yield_now();
    }
    cancellation.interrupt();
    locks.wake_waiters();
    assert_eq!(waiter.join().unwrap(), Err(LockError::Interrupted));
    assert_eq!(locks.waiting(), 0);
    assert_eq!(locks.snapshot().flocks[0].owner, LockFixture::ofd(1));
}

#[test]
fn interrupt_before_range_wait() {
    let locks = AdvisoryLockCoordinator::new();
    let cancellation = LockCancellation::default();
    locks.interrupt(&cancellation);
    assert_eq!(
        locks.set_range(
            LockFixture::file(1),
            LockFixture::process(1),
            Some(RangeLockKind::Write),
            LockFixture::range(0, Some(1)),
            true,
            false,
            &cancellation,
        ),
        Ok(())
    );
}

#[test]
fn interrupt_after_range_wait() {
    let locks = Arc::new(AdvisoryLockCoordinator::new());
    locks
        .set_range(
            LockFixture::file(1),
            LockFixture::process(1),
            Some(RangeLockKind::Write),
            LockFixture::range(0, Some(1)),
            false,
            false,
            &LockCancellation::default(),
        )
        .unwrap();
    let cancellation = Arc::new(LockCancellation::default());
    let waiting = Arc::clone(&locks);
    let waiting_cancellation = Arc::clone(&cancellation);
    let waiter = thread::spawn(move || {
        waiting.set_range(
            LockFixture::file(1),
            LockFixture::process(2),
            Some(RangeLockKind::Write),
            LockFixture::range(0, Some(1)),
            true,
            false,
            &waiting_cancellation,
        )
    });
    while locks.waiting() != 1 {
        thread::yield_now();
    }
    locks.interrupt(&cancellation);
    assert_eq!(waiter.join().unwrap(), Err(LockError::Interrupted));
    assert_eq!(locks.waiting(), 0);
}

#[test]
fn pointer_free_publish() {
    let source = AdvisoryLockCoordinator::new();
    source
        .set_flock(
            LockFixture::file(1),
            LockFixture::ofd(1),
            Some(FlockMode::Exclusive),
            false,
            &LockCancellation::default(),
        )
        .unwrap();
    source
        .set_range(
            LockFixture::file(2),
            LockFixture::process(1),
            Some(RangeLockKind::Read),
            LockFixture::range(4, None),
            false,
            false,
            &LockCancellation::default(),
        )
        .unwrap();
    let snapshot = source.snapshot();
    let restored = AdvisoryLockCoordinator::new();
    restored.restore(&snapshot).unwrap();
    assert_eq!(restored.snapshot(), snapshot);

    let mut invalid = snapshot;
    invalid.flocks.push(invalid.flocks[0]);
    let rejected = AdvisoryLockCoordinator::new();
    assert_eq!(rejected.restore(&invalid), Err(LockError::InvalidArgument));
    assert_eq!(rejected.snapshot(), super::LockSnapshot::default());
}

#[test]
fn thousand_operation_merge() {
    let locks = AdvisoryLockCoordinator::new();
    let owner = LockFixture::process(1);
    let cancel = LockCancellation::default();
    let mut model = [false; 64];
    let mut seed = 0x9e37_79b9_u32;
    for step in 0..1000 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let start = (seed as usize >> 8) % model.len();
        let width = ((seed as usize >> 24) % 12 + 1).min(model.len() - start);
        let end = start + width;
        let lock = step % 3 != 0;
        locks
            .set_range(
                LockFixture::file(1),
                owner,
                lock.then_some(RangeLockKind::Write),
                LockFixture::range(start as u64, Some(end as u64)),
                false,
                false,
                &cancel,
            )
            .unwrap();
        model[start..end].fill(lock);
        let snapshot = locks.snapshot();
        let mut observed = [false; 64];
        for record in snapshot.ranges {
            let record_end = record.range.end.unwrap_or(64).min(64) as usize;
            observed[record.range.start as usize..record_end].fill(true);
        }
        assert_eq!(observed, model);
    }
}
