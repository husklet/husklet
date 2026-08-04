use std::sync::{Arc, Barrier};
use std::time::Duration;

use crate::{AioError, Catalog, CatalogLimits, Event};

fn event(value: i64) -> Event {
    Event {
        data: value as u64,
        object: 7,
        result: value,
        secondary: 0,
    }
}

#[test]
fn capacity_backpressure() {
    let catalog = Catalog::new(CatalogLimits {
        contexts: 1,
        events_per_context: 2,
    });
    let id = catalog.create(2).unwrap();
    catalog.admit(id).unwrap().complete(event(1)).unwrap();
    catalog.admit(id).unwrap().complete(event(2)).unwrap();
    assert!(matches!(catalog.admit(id), Err(AioError::ResourceLimit)));
    let batch = catalog.stage(id, 0, 2, Some(Duration::ZERO), || false).unwrap();
    assert_eq!(batch.events(), &[event(1), event(2)]);
    batch.commit();
}

#[test]
fn copyout_preserves_events() {
    let catalog = Catalog::default();
    let id = catalog.create(1).unwrap();
    catalog.admit(id).unwrap().complete(event(4)).unwrap();
    drop(catalog.stage(id, 1, 1, None, || false).unwrap());
    let batch = catalog.stage(id, 1, 1, None, || false).unwrap();
    assert_eq!(batch.events(), &[event(4)]);
}

#[test]
fn rejects_stale_generation() {
    let catalog = Catalog::new(CatalogLimits {
        contexts: 1,
        events_per_context: 1,
    });
    let old = catalog.create(1).unwrap();
    catalog.destroy(old).unwrap();
    let current = catalog.create(1).unwrap();
    assert_ne!(old, current);
    assert!(matches!(catalog.admit(old), Err(AioError::InvalidArgument)));
}

#[test]
fn destroy_quiesces_admission() {
    let catalog = Arc::new(Catalog::default());
    let id = catalog.create(1).unwrap();
    let admission = catalog.admit(id).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let worker_catalog = Arc::clone(&catalog);
    let worker_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        worker_catalog.destroy(id)
    });
    barrier.wait();
    drop(admission);
    assert_eq!(worker.join().unwrap(), Ok(()));
}
