use crate::{SharedBackingRef, SharedError, SharedLimits, SharedObjectStore, SharedSeal};
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[test]
fn aliases_are_coherent() {
    let store = SharedObjectStore::new(SharedLimits::default()).unwrap();
    let id = store.create(7, 8).unwrap();
    let writer = store.pin(id, true).unwrap();
    let reader = store.pin(id, false).unwrap();
    writer.write(2, b"rust").unwrap();
    let mut bytes = [0; 4];
    reader.read(2, &mut bytes).unwrap();
    assert_eq!(&bytes, b"rust");
    store.remove(id).unwrap();
    assert_eq!(store.pin(id, false).unwrap_err(), SharedError::NotFound);
    reader.read(2, &mut bytes).unwrap();
    let inherited = store
        .pin_inherited(
            SharedBackingRef {
                object: id,
                offset: 0,
                length: 8,
                write_shared: false,
            },
            false,
        )
        .unwrap();
    inherited.read(2, &mut bytes).unwrap();
    drop(inherited);
    drop(reader);
    drop(writer);
    let replacement = store.create(8, 8).unwrap();
    assert_eq!(replacement.slot, id.slot);
    assert_ne!(replacement.generation, id.generation);
}

#[test]
fn seals_enforce_busy() {
    let store = SharedObjectStore::new(SharedLimits::default()).unwrap();
    let id = store.create(1, 4).unwrap();
    let writer = store.pin(id, true).unwrap();
    assert_eq!(
        store.add_seals(id, SharedSeal::from_bits(SharedSeal::WRITE)),
        Err(SharedError::Busy)
    );
    store
        .add_seals(id, SharedSeal::from_bits(SharedSeal::FUTURE_WRITE))
        .unwrap();
    assert_eq!(store.pin(id, true).unwrap_err(), SharedError::Sealed);
    writer.write(0, b"x").unwrap();
    drop(writer);
    store
        .add_seals(id, SharedSeal::from_bits(SharedSeal::WRITE | SharedSeal::SEAL))
        .unwrap();
    assert_eq!(
        store.add_seals(id, SharedSeal::from_bits(SharedSeal::GROW)),
        Err(SharedError::Sealed)
    );
}

#[test]
fn write_seal_distinguishes_shared_from_private_maps() {
    let store = SharedObjectStore::new(SharedLimits::default()).unwrap();
    let private = store.create(1, 4096).unwrap();
    let private_pin = store
        .pin_backing(
            SharedBackingRef {
                object: private,
                offset: 0,
                length: 4096,
                write_shared: false,
            },
            false,
        )
        .unwrap();
    assert!(store.add_seals(private, SharedSeal::from_bits(SharedSeal::WRITE)).is_ok());
    drop(private_pin);

    let shared = store.create(1, 4096).unwrap();
    let shared_pin = store
        .pin_backing(
            SharedBackingRef {
                object: shared,
                offset: 0,
                length: 4096,
                write_shared: true,
            },
            false,
        )
        .unwrap();
    assert_eq!(
        store.add_seals(shared, SharedSeal::from_bits(SharedSeal::WRITE)),
        Err(SharedError::Busy)
    );
    drop(shared_pin);
    assert!(store.add_seals(shared, SharedSeal::from_bits(SharedSeal::WRITE)).is_ok());
}

#[test]
fn truncate_seals_snapshot() {
    let store = SharedObjectStore::new(SharedLimits::default()).unwrap();
    let old = store.create(4, 4).unwrap();
    store.resize(old, 8).unwrap();
    store.add_seals(old, SharedSeal::from_bits(SharedSeal::SHRINK)).unwrap();
    assert_eq!(store.resize(old, 2), Err(SharedError::Sealed));
    let snapshot = store.snapshot();
    let restored = SharedObjectStore::restore(SharedLimits::default(), snapshot).unwrap();
    assert_eq!(restored.pin(old, false).unwrap().len(), 8);
    store.remove(old).unwrap();
    let new = store.create(5, 1).unwrap();
    assert_eq!(new.slot, old.slot);
    assert_ne!(new.generation, old.generation);
}

#[test]
fn vacant_slot_generation() {
    let store = SharedObjectStore::new(SharedLimits::default()).unwrap();
    let stale = store.create(1, 4).unwrap();
    store.remove(stale).unwrap();
    let snapshot = store.snapshot();
    let expected = snapshot.generations[stale.slot as usize];
    let restored = SharedObjectStore::restore(SharedLimits::default(), snapshot).unwrap();
    let current = restored.create(2, 4).unwrap();
    assert_eq!(current.slot, stale.slot);
    assert_eq!(current.generation, expected);
    assert_ne!(current.generation, stale.generation);
}

#[test]
fn frozen_store_blocks() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let id = store.create(1, 4).unwrap();
    let pin = store.pin(id, true).unwrap();
    store.freeze_checkpoint();
    let (sent, received) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        pin.write(0, b"x").unwrap();
        sent.send(()).unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
    assert_eq!(store.checkpoint_snapshot().unwrap().objects[0].bytes, [0; 4]);
    store.thaw_checkpoint();
    received.recv_timeout(Duration::from_secs(1)).unwrap();
    worker.join().unwrap();
}

#[test]
fn limits_fail_without() {
    let limits = SharedLimits {
        objects: 1,
        object_bytes: 4,
        total_bytes: 4,
    };
    let store = SharedObjectStore::new(limits).unwrap();
    let id = store.create(1, 4).unwrap();
    assert_eq!(store.create(2, 1), Err(SharedError::ResourceLimit));
    assert_eq!(store.resize(id, 5), Err(SharedError::ResourceLimit));
    assert_eq!(store.pin(id, false).unwrap().len(), 4);
}
