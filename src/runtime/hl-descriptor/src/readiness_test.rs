use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use crate::{DescriptorFlags, DescriptorTable, ObjectError, OpenFileDescription, ReadinessObserver, ReadinessRegistry};

struct CountingSubscription(Arc<AtomicUsize>);

impl crate::ReadinessSubscription for CountingSubscription {
    fn quiesce(&self) {}
}

impl Drop for CountingSubscription {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct TestDescription;

impl OpenFileDescription for TestDescription {}

#[derive(Debug)]
struct NoopObserver;

impl ReadinessObserver for NoopObserver {
    fn readiness_changed(&self) {}
}

#[derive(Debug)]
struct ReadyDescription {
    registry: ReadinessRegistry,
}

impl OpenFileDescription for ReadyDescription {
    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn crate::ReadinessSubscription>, ObjectError> {
        self.registry.subscribe(observer)
    }

    fn retire(&self) {
        self.registry.close();
    }
}

struct CountingObserver(Arc<AtomicUsize>);

impl ReadinessObserver for CountingObserver {
    fn readiness_changed(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn operation_pin_identity() {
    let table = DescriptorTable::new(8).unwrap();
    let first = table
        .install(0, Arc::new(TestDescription), DescriptorFlags::default())
        .unwrap();
    let alias = table.duplicate(first, 0, DescriptorFlags::default()).unwrap();
    let first_identity = table.pin(first).unwrap().description_identity();
    assert_eq!(table.pin(alias).unwrap().description_identity(), first_identity);
    table.close(first).unwrap();
    table.close(alias).unwrap();
    let reused = table
        .install(0, Arc::new(TestDescription), DescriptorFlags::default())
        .unwrap();
    assert_eq!(reused, first);
    assert_ne!(table.pin(reused).unwrap().description_identity(), first_identity);
}

#[test]
fn readiness_subscription_is() {
    let table = DescriptorTable::new(2).unwrap();
    let number = table
        .install(0, Arc::new(TestDescription), DescriptorFlags::default())
        .unwrap();
    let result = table.pin(number).unwrap().subscribe_readiness(Arc::new(NoopObserver));
    assert!(matches!(result, Err(ObjectError::NotSupported)));
}

#[test]
fn async_ofd_lifecycle() {
    let registry = ReadinessRegistry::new();
    let table = DescriptorTable::new(4).unwrap();
    let number = table
        .install(
            0,
            Arc::new(ReadyDescription {
                registry: registry.clone(),
            }),
            DescriptorFlags::default(),
        )
        .unwrap();
    let alias = table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    let child = table.fork();
    let calls = Arc::new(AtomicUsize::new(0));
    table
        .pin(number)
        .unwrap()
        .set_async_observer(Some(Arc::new(CountingObserver(Arc::clone(&calls)))))
        .unwrap();

    registry.notify();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    table.close(number).unwrap();
    table.close(alias).unwrap();
    registry.notify();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    child.close(number).unwrap();
    child.close(alias).unwrap();
    registry.notify();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn notify_ofd_lifecycle() {
    let table = DescriptorTable::new(4).unwrap();
    let number = table
        .install(0, Arc::new(TestDescription), DescriptorFlags::default())
        .unwrap();
    let alias = table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    let child = table.fork();
    let calls = Arc::new(AtomicUsize::new(0));
    table
        .pin(number)
        .unwrap()
        .set_notify_subscription(Some(Box::new(CountingSubscription(Arc::clone(&calls)))));
    table.close(number).unwrap();
    table.close(alias).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    child.close(number).unwrap();
    child.close(alias).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct BlockingObserver {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    calls: Arc<AtomicUsize>,
}

impl ReadinessObserver for BlockingObserver {
    fn readiness_changed(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.wait();
        self.release.wait();
    }
}

#[test]
fn quiesce_waits_for() {
    let registry = ReadinessRegistry::new();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    let subscription = registry
        .subscribe(Arc::new(BlockingObserver {
            entered: entered.clone(),
            release: release.clone(),
            calls: calls.clone(),
        }))
        .unwrap();
    let notifier = registry.clone();
    let notify_thread = thread::spawn(move || notifier.notify());
    entered.wait();
    let (sent, received) = mpsc::channel();
    let quiesce_thread = thread::spawn(move || {
        subscription.quiesce();
        sent.send(()).unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(10)).is_err());
    release.wait();
    notify_thread.join().unwrap();
    quiesce_thread.join().unwrap();
    registry.notify();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn drop_joins_callback() {
    let registry = ReadinessRegistry::new();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    let subscription = registry
        .subscribe(Arc::new(BlockingObserver {
            entered: entered.clone(),
            release: release.clone(),
            calls: calls.clone(),
        }))
        .unwrap();
    let notifier = registry.clone();
    let notify_thread = thread::spawn(move || notifier.notify());
    entered.wait();
    let (sent, received) = mpsc::channel();
    let drop_thread = thread::spawn(move || {
        drop(subscription);
        sent.send(()).unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(10)).is_err());
    release.wait();
    notify_thread.join().unwrap();
    drop_thread.join().unwrap();
    registry.notify();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct SelfRemovingObserver {
    subscription: Arc<std::sync::Mutex<Option<Box<dyn crate::ReadinessSubscription>>>>,
    calls: Arc<AtomicUsize>,
}

impl ReadinessObserver for SelfRemovingObserver {
    fn readiness_changed(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.subscription.lock().unwrap().take();
    }
}

#[test]
fn callback_drops_itself() {
    let registry = ReadinessRegistry::new();
    let subscription = Arc::new(std::sync::Mutex::new(None));
    let calls = Arc::new(AtomicUsize::new(0));
    let observer = Arc::new(SelfRemovingObserver {
        subscription: subscription.clone(),
        calls: calls.clone(),
    });
    *subscription.lock().unwrap() = Some(registry.subscribe(observer).unwrap());
    registry.notify();
    registry.notify();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
