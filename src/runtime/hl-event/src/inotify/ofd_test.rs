use hl_descriptor::{
    CancellationNotification, CancellationSubscription, DescriptorFlags, DescriptorTable, ObjectError,
    OperationCancellation, OperationContext, StatusFlags,
};

struct Cancellation {
    pending: std::sync::atomic::AtomicBool,
    notifications: std::sync::Mutex<Vec<std::sync::Arc<dyn CancellationNotification>>>,
}

struct Subscription;

impl CancellationSubscription for Subscription {}

impl Cancellation {
    fn new() -> Self {
        Self {
            pending: std::sync::atomic::AtomicBool::new(false),
            notifications: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn interrupt(&self) {
        self.pending.store(true, std::sync::atomic::Ordering::Release);
        for notification in self.notifications.lock().unwrap().iter() {
            notification.notify();
        }
    }
}

impl OperationCancellation for Cancellation {
    fn interrupted(&self) -> bool {
        self.pending.swap(false, std::sync::atomic::Ordering::AcqRel)
    }

    fn subscribe(
        &self,
        notification: std::sync::Arc<dyn CancellationNotification>,
    ) -> Box<dyn CancellationSubscription> {
        self.notifications.lock().unwrap().push(notification);
        Box::new(Subscription)
    }
}

use super::test_support::Fixture;
use crate::InotifyMask;

#[test]
fn descriptor_nonblocking_remove() {
    let fixture = Fixture::new(false);
    let descriptor = fixture.watch(b"/file", InotifyMask::MODIFY);
    let table = DescriptorTable::new(2).unwrap();
    let number = table
        .commit(
            table.reserve(0).unwrap(),
            fixture.inotify.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let lease = table.pin(number).unwrap();
    lease
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    assert_eq!(lease.read(&mut [0_u8; 32]), Err(ObjectError::WouldBlock));
    fixture.inotify.remove_watch(descriptor).unwrap();
    let mut event = [0_u8; 16];
    assert_eq!(lease.read(&mut event), Ok(16));
    assert_eq!(Fixture::u32_at(&event, 4), InotifyMask::IGNORED);
    let snapshot = fixture.inotify.snapshot();
    assert!(snapshot.watches.is_empty());
    assert!(snapshot.nonblocking);
    assert_eq!(fixture.inotify.status().mode, 0o100_600);
}

#[test]
fn prepared_read_events() {
    let fixture = Fixture::new(true);
    fixture.watch(b"/dir", InotifyMask::CREATE);
    fixture.emit(InotifyMask::CREATE, b"first");
    let prepared = fixture.inotify.prepare_read(1_024).unwrap();
    fixture.emit(InotifyMask::CREATE, b"second");
    fixture.inotify.commit_read(&prepared).unwrap();
    let remaining = fixture.read_all();
    assert!(remaining.windows(6).any(|window| window == b"second"));
    assert!(!remaining.windows(5).any(|window| window == b"first"));
}

#[test]
fn blocking_read_interrupted() {
    let fixture = Fixture::new(false);
    let table = std::sync::Arc::new(DescriptorTable::new(2).unwrap());
    let number = table
        .commit(
            table.reserve(0).unwrap(),
            fixture.inotify,
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let cancellation = std::sync::Arc::new(Cancellation::new());
    let blocked = {
        let table = table.clone();
        let cancellation = cancellation.clone();
        std::thread::spawn(move || {
            let lease = table.pin(number).unwrap();
            lease
                .prepare_atomic_context(
                    32,
                    OperationContext {
                        actor: None,
                        cancellation: Some(cancellation.as_ref()),
                    },
                )
                .map(|_| ())
        })
    };
    while cancellation.notifications.lock().unwrap().is_empty() {
        std::thread::yield_now();
    }
    cancellation.interrupt();
    assert_eq!(blocked.join().unwrap(), Err(ObjectError::Interrupted));
}
