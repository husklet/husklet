use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{DescriptorError, DescriptorFlags, DescriptorTable, OpenFileDescription, StatusFlags};

#[derive(Debug, Default)]
struct TransferLifecycle {
    retired: AtomicUsize,
    closed: AtomicUsize,
}

impl OpenFileDescription for TransferLifecycle {
    fn retire(&self) {
        self.retired.fetch_add(1, Ordering::Relaxed);
    }

    fn close(&self) {
        self.closed.fetch_add(1, Ordering::Relaxed);
    }
}

impl TransferLifecycle {
    fn batch_object(object: Arc<Self>) -> (Arc<dyn OpenFileDescription>, StatusFlags, DescriptorFlags) {
        (object, StatusFlags::default(), DescriptorFlags::default())
    }
}

#[test]
fn queued_transfer_outlives() {
    let lifecycle = Arc::new(TransferLifecycle::default());
    let sender = DescriptorTable::new(8).unwrap();
    let source = sender
        .install(0, lifecycle.clone(), DescriptorFlags::default())
        .unwrap();
    let queued = sender.export_description(source).unwrap();

    sender.close(source).unwrap();
    assert_eq!(lifecycle.retired.load(Ordering::Relaxed), 0);

    let receiver = DescriptorTable::new(8).unwrap();
    let installed = receiver
        .install_description(0, &queued, DescriptorFlags::default())
        .unwrap();
    drop(queued);
    assert_eq!(lifecycle.retired.load(Ordering::Relaxed), 0);

    receiver.close(installed).unwrap();
    assert_eq!(lifecycle.retired.load(Ordering::Relaxed), 1);
    assert_eq!(lifecycle.closed.load(Ordering::Relaxed), 1);
}

#[test]
fn prepared_open_is() {
    let table = DescriptorTable::new(4).unwrap();
    let object = Arc::new(TransferLifecycle::default());
    let prepared = table
        .prepare_open(0, object.clone(), StatusFlags::default(), DescriptorFlags::default())
        .unwrap();
    assert_eq!(prepared.number(), 0);
    assert_eq!(table.pin(0).unwrap_err(), DescriptorError::BadDescriptor);
    drop(prepared);
    let replacement = table.install(0, object, DescriptorFlags::default()).unwrap();
    assert_eq!(replacement, 0);
}

#[test]
fn prepared_reference_count() {
    let table = DescriptorTable::new(4).unwrap();
    let object = Arc::new(TransferLifecycle::default());
    let prepared = table
        .prepare_open(0, object.clone(), StatusFlags::default(), DescriptorFlags::default())
        .unwrap();
    let number = prepared.publish();
    assert_eq!(table.snapshot(number).unwrap().descriptor_references, 1);
    let alias = table.duplicate(number, 0, DescriptorFlags::default()).unwrap();
    assert_eq!(table.snapshot(number).unwrap().descriptor_references, 2);
    table.close(number).unwrap();
    assert_eq!(table.snapshot(alias).unwrap().descriptor_references, 1);
    table.close(alias).unwrap();
    assert_eq!(object.retired.load(Ordering::Relaxed), 1);
}

#[test]
fn checkpoint_freeze_waits() {
    let table = DescriptorTable::new(4).unwrap();
    let object = Arc::new(TransferLifecycle::default());
    let prepared = table
        .prepare_open(0, object, StatusFlags::default(), DescriptorFlags::default())
        .unwrap();
    std::thread::scope(|scope| {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (frozen_tx, frozen_rx) = std::sync::mpsc::channel();
        let table_ref = &table;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            table_ref.freeze_checkpoint();
            frozen_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(frozen_rx.try_recv().is_err());
        assert_eq!(prepared.publish(), 0);
        frozen_rx.recv().unwrap();
        assert_eq!(table.pin(0).unwrap_err(), DescriptorError::CheckpointFrozen);
        table.thaw_checkpoint();
    });
    assert_eq!(table.pin(0).unwrap().descriptor_number(), 0);
}

#[test]
fn prepared_open_batch() {
    let table = DescriptorTable::new(3).unwrap();
    let object = Arc::new(TransferLifecycle::default());
    let prepared = table
        .prepare_open_batch(
            0,
            vec![
                TransferLifecycle::batch_object(object.clone()),
                TransferLifecycle::batch_object(object.clone()),
            ],
        )
        .unwrap();
    assert_eq!(prepared.numbers(), [0, 1]);
    assert_eq!(table.pin(0).unwrap_err(), DescriptorError::BadDescriptor);
    assert_eq!(table.pin(1).unwrap_err(), DescriptorError::BadDescriptor);
    drop(prepared);
    assert_eq!(table.install(0, object.clone(), DescriptorFlags::default()).unwrap(), 0);
    assert_eq!(table.install(0, object, DescriptorFlags::default()).unwrap(), 1);
}

#[test]
fn prepared_open_nothing() {
    let table = DescriptorTable::new(1).unwrap();
    let object = Arc::new(TransferLifecycle::default());
    assert!(matches!(
        table.prepare_open_batch(
            0,
            vec![
                TransferLifecycle::batch_object(object.clone()),
                TransferLifecycle::batch_object(object.clone()),
            ],
        ),
        Err(DescriptorError::TooManyOpenFiles),
    ));
    assert_eq!(table.install(0, object, DescriptorFlags::default()).unwrap(), 0);
}

#[test]
fn checkpoint_freeze_publication() {
    let table = DescriptorTable::new(2).unwrap();
    let object = Arc::new(TransferLifecycle::default());
    let prepared = table
        .prepare_open_batch(
            0,
            vec![
                TransferLifecycle::batch_object(object.clone()),
                TransferLifecycle::batch_object(object),
            ],
        )
        .unwrap();
    std::thread::scope(|scope| {
        let (frozen_tx, frozen_rx) = std::sync::mpsc::channel();
        let table_ref = &table;
        scope.spawn(move || {
            table_ref.freeze_checkpoint();
            frozen_tx.send(()).unwrap();
        });
        assert!(frozen_rx.try_recv().is_err());
        assert_eq!(prepared.publish_all(), [0, 1]);
        frozen_rx.recv().unwrap();
        table.thaw_checkpoint();
    });
    assert!(table.pin(0).is_ok());
    assert!(table.pin(1).is_ok());
}
