use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use hl_descriptor::{
    CancellationNotification, CancellationSubscription, DescriptorFlags, DescriptorTable, ObjectError, ObjectKind,
    OperationCancellation, OperationContext, Readiness, StatusFlags,
};

use crate::{EventFd, EventFdError, EventFdFlags, EventInterest};

fn bytes(value: u64) -> [u8; 8] {
    value.to_ne_bytes()
}

fn write_many(eventfd: &EventFd, count: usize) {
    for _ in 0..count {
        eventfd.write(&bytes(1)).unwrap();
    }
}

struct Cancellation {
    pending: std::sync::atomic::AtomicBool,
    notifications: std::sync::Mutex<Vec<Arc<dyn CancellationNotification>>>,
}

struct CancellationGuard;

impl CancellationSubscription for CancellationGuard {}

impl Cancellation {
    fn new() -> Self {
        Self {
            pending: std::sync::atomic::AtomicBool::new(false),
            notifications: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn interrupt(&self) {
        self.pending.store(true, Ordering::Release);
        for notification in self.notifications.lock().unwrap().iter() {
            notification.notify();
        }
    }
}

impl OperationCancellation for Cancellation {
    fn interrupted(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    fn subscribe(&self, notification: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
        self.notifications.lock().unwrap().push(notification);
        Box::new(CancellationGuard)
    }
}

#[test]
fn snapshot_restores_state() {
    let event = EventFd::new(
        9,
        EventFdFlags::from_bits(EventFdFlags::SEMAPHORE | EventFdFlags::NONBLOCKING),
    )
    .unwrap();
    let snapshot = event.snapshot();
    let restored = EventFd::from_snapshot(snapshot).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
    let mut output = [0; 8];
    restored.read(&mut output).unwrap();
    assert_eq!(u64::from_ne_bytes(output), 1);
    assert_eq!(restored.counter(), 8);
}

#[test]
fn creation_validates_value() {
    assert_eq!(
        EventFd::new(u64::MAX, EventFdFlags::default()).unwrap_err(),
        EventFdError::InvalidArgument
    );
    assert_eq!(
        EventFd::new(0, EventFdFlags::from_bits(4)).unwrap_err(),
        EventFdError::InvalidArgument
    );
}

#[test]
fn ordinary_reads_counter() {
    let eventfd = EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap();
    eventfd.write(&bytes(4)).unwrap();
    eventfd.write(&bytes(5)).unwrap();
    let mut output = [0_u8; 8];
    assert_eq!(eventfd.read(&mut output), Ok(8));
    assert_eq!(u64::from_ne_bytes(output), 9);
    assert_eq!(eventfd.read(&mut output), Err(EventFdError::WouldBlock));
}

#[test]
fn semaphore_reads_empty() {
    let eventfd = EventFd::new(
        3,
        EventFdFlags::from_bits(EventFdFlags::SEMAPHORE | EventFdFlags::NONBLOCKING),
    )
    .unwrap();
    let mut output = [0_u8; 8];
    for _ in 0..3 {
        assert_eq!(eventfd.read(&mut output), Ok(8));
        assert_eq!(u64::from_ne_bytes(output), 1);
    }
    assert_eq!(eventfd.read(&mut output), Err(EventFdError::WouldBlock));
}

#[test]
fn read_write_asymmetric() {
    let eventfd = EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap();
    assert_eq!(eventfd.write(&[0; 4]), Err(EventFdError::InvalidArgument));
    assert_eq!(eventfd.write(&[0; 9]), Err(EventFdError::InvalidArgument));
    eventfd.write(&bytes(5)).unwrap();
    let mut short = [0_u8; 4];
    assert_eq!(eventfd.read(&mut short), Err(EventFdError::InvalidArgument));
    assert_eq!(eventfd.counter(), 5);
    let mut wide = [0xaa; 16];
    assert_eq!(eventfd.read(&mut wide), Ok(8));
    assert_eq!(u64::from_ne_bytes(wide[..8].try_into().unwrap()), 5);
    assert_eq!(&wide[8..], &[0xaa; 8]);
}

#[test]
fn zero_noop_invalid() {
    let eventfd = EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap();
    assert_eq!(eventfd.write(&bytes(0)), Ok(8));
    assert_eq!(eventfd.counter(), 0);
    assert_eq!(eventfd.write(&bytes(u64::MAX)), Err(EventFdError::InvalidArgument));
}

#[test]
fn overflow_eagain_counter() {
    let eventfd = EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap();
    assert_eq!(eventfd.write(&bytes(u64::MAX - 1)), Ok(8));
    assert_eq!(eventfd.write(&bytes(1)), Err(EventFdError::WouldBlock));
    assert_eq!(eventfd.counter(), u64::MAX - 1);
}

#[test]
fn saturation_readiness() {
    let eventfd = EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap();
    let interests = EventInterest::from_bits(EventInterest::READ | EventInterest::WRITE);

    assert_eq!(
        eventfd.readiness(interests),
        EventInterest::from_bits(EventInterest::WRITE)
    );
    eventfd.write(&bytes(u64::MAX - 1)).unwrap();
    assert_eq!(
        eventfd.readiness(interests),
        EventInterest::from_bits(EventInterest::READ)
    );

    let mut output = [0_u8; 8];
    eventfd.read(&mut output).unwrap();
    assert_eq!(
        eventfd.readiness(interests),
        EventInterest::from_bits(EventInterest::WRITE)
    );
}

#[test]
fn blocking_read_write() {
    let eventfd = EventFd::new(0, EventFdFlags::default()).unwrap();
    let reader = eventfd.clone();
    let started = Arc::new(Barrier::new(2));
    let reader_started = started.clone();
    let thread = thread::spawn(move || {
        reader_started.wait();
        let mut output = [0_u8; 8];
        reader.read(&mut output).map(|_| u64::from_ne_bytes(output))
    });
    started.wait();
    thread::sleep(Duration::from_millis(10));
    eventfd.write(&bytes(7)).unwrap();
    assert_eq!(thread.join().unwrap(), Ok(7));
}

#[test]
fn blocked_overflow_drained() {
    let eventfd = EventFd::new(u64::MAX - 1, EventFdFlags::default()).unwrap();
    let writer = eventfd.clone();
    let started = Arc::new(Barrier::new(2));
    let writer_started = started.clone();
    let thread = thread::spawn(move || {
        writer_started.wait();
        writer.write(&bytes(1))
    });
    started.wait();
    thread::sleep(Duration::from_millis(10));
    assert_eq!(eventfd.counter(), u64::MAX - 1);
    let mut output = [0_u8; 8];
    assert_eq!(eventfd.read(&mut output), Ok(8));
    assert_eq!(thread.join().unwrap(), Ok(8));
    assert_eq!(eventfd.counter(), 1);
}

#[test]
fn concurrent_writes_lost() {
    const THREADS: usize = 8;
    const WRITES: usize = 1_000;
    let eventfd = EventFd::new(0, EventFdFlags::default()).unwrap();
    let mut threads = Vec::new();
    for _ in 0..THREADS {
        let writer = eventfd.clone();
        threads.push(thread::spawn(move || write_many(&writer, WRITES)));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let mut output = [0_u8; 8];
    assert_eq!(eventfd.read(&mut output), Ok(8));
    assert_eq!(u64::from_ne_bytes(output), u64::try_from(THREADS * WRITES).unwrap());
}

#[test]
fn readiness_status_eventfd() {
    let eventfd = EventFd::new(0, EventFdFlags::default()).unwrap();
    let interests = EventInterest::from_bits(EventInterest::READ | EventInterest::WRITE);
    assert_eq!(eventfd.readiness(interests).bits(), EventInterest::WRITE);
    eventfd.write(&bytes(1)).unwrap();
    assert_eq!(
        eventfd.readiness(interests).bits(),
        EventInterest::READ | EventInterest::WRITE
    );
    assert_eq!(eventfd.status().mode, 0o100_600);
    assert_eq!(eventfd.status().size, 0);
    assert_eq!(eventfd.status().link_count, 1);
}

#[test]
fn subscription_inline_notifications() {
    let eventfd = EventFd::new(0, EventFdFlags::default()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let subscription = eventfd
        .subscribe(
            17,
            Arc::new(move |token| {
                assert_eq!(token, 17);
                observed.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    eventfd.write(&bytes(1)).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    drop(subscription);
    eventfd.write(&bytes(1)).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn descriptor_retirement_retired() {
    let table = DescriptorTable::new(8).unwrap();
    let eventfd = Arc::new(EventFd::new(0, EventFdFlags::default()).unwrap());
    let number = table
        .commit(
            table.reserve(0).unwrap(),
            eventfd.clone(),
            StatusFlags::from_bits(0o2),
            DescriptorFlags::default(),
        )
        .unwrap();
    assert_eq!(table.snapshot(number).unwrap().kind, ObjectKind::EventCounter);
    let lease = table.pin(number).unwrap();
    let waiter = eventfd.clone();
    let thread = thread::spawn(move || {
        let mut output = [0_u8; 8];
        waiter.read(&mut output)
    });
    thread::sleep(Duration::from_millis(10));
    table.close(number).unwrap();
    assert!(lease.retired());
    assert_eq!(thread.join().unwrap(), Err(EventFdError::Retired));
    drop(lease);
    assert!(eventfd.is_retired());
}

#[test]
fn descriptor_operation_downcasting() {
    let eventfd = EventFd::new(0, EventFdFlags::default()).unwrap();
    let table = DescriptorTable::new(8).unwrap();
    let number = table
        .install(0, Arc::new(eventfd.clone()), DescriptorFlags::default())
        .unwrap();
    let lease = table.pin(number).unwrap();

    lease
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    let mut output = [0_u8; 8];
    assert_eq!(lease.read(&mut output), Err(ObjectError::WouldBlock));
    assert_eq!(lease.write(&bytes(9)), Ok(8));
    assert!(
        lease
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
    assert_eq!(lease.read(&mut output), Ok(8));
    assert_eq!(u64::from_ne_bytes(output), 9);
}

#[test]
fn nonblocking_flag_toggled() {
    let eventfd = EventFd::new(0, EventFdFlags::from_bits(EventFdFlags::NONBLOCKING)).unwrap();
    let mut output = [0_u8; 8];
    assert_eq!(eventfd.read(&mut output), Err(EventFdError::WouldBlock));
    eventfd.set_nonblocking(false).unwrap();
    let writer = eventfd.clone();
    let thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        writer.write(&bytes(2)).unwrap();
    });
    assert_eq!(eventfd.read(&mut output), Ok(8));
    assert_eq!(u64::from_ne_bytes(output), 2);
    thread.join().unwrap();
}

#[test]
fn blocked_operations_interrupted() {
    for (initial, write) in [(0, false), (u64::MAX - 1, true)] {
        assert_interrupted(initial, write);
    }
}

fn assert_interrupted(initial: u64, write: bool) {
    let eventfd = Arc::new(EventFd::new(initial, EventFdFlags::default()).unwrap());
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let number = table.install(0, eventfd.clone(), DescriptorFlags::default()).unwrap();
    let cancellation = Arc::new(Cancellation::new());
    let blocked = {
        let table = table.clone();
        let cancellation = cancellation.clone();
        thread::spawn(move || {
            let lease = table.pin(number).unwrap();
            let context = OperationContext {
                actor: None,
                cancellation: Some(cancellation.as_ref()),
            };
            if write {
                lease.write_context(&bytes(1), context)
            } else {
                lease.read_context(&mut [0_u8; 8], context)
            }
        })
    };
    while cancellation.notifications.lock().unwrap().is_empty() {
        thread::yield_now();
    }
    cancellation.interrupt();
    assert_eq!(blocked.join().unwrap(), Err(ObjectError::Interrupted));
    assert_eq!(eventfd.counter(), initial);
}
