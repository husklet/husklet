use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::Duration;

use hl_gpu::runtime::model::sharing::SessionId;
use hl_gpu::{GpuError, SharedSync, SyncExportId, SyncExports, TimelineSync, TimelineWait};

const OWNER: SessionId = SessionId(1);
const IMPORTER: SessionId = SessionId(2);
const OTHER: SessionId = SessionId(3);

fn object(value: u64) -> SharedSync {
    Arc::new(value)
}

fn value(object: &SharedSync) -> u64 {
    *object.downcast_ref::<u64>().unwrap()
}

#[test]
fn identities_are_monotonic_and_never_reused() {
    let exports = SyncExports::new();
    let first = exports.export(OWNER, object(1)).unwrap();
    exports.owner_release(OWNER, first).unwrap();
    let second = exports.export(OWNER, object(2)).unwrap();

    assert!(second > first);
    assert_eq!(
        exports.import(IMPORTER, first).unwrap_err(),
        GpuError::Invalid("stale synchronization export")
    );
    assert_eq!(value(&exports.import(IMPORTER, second).unwrap()), 2);
}

#[test]
fn self_and_duplicate_imports_are_refused_but_distinct_importers_work() {
    let exports = SyncExports::new();
    let id = exports.export(OWNER, object(7)).unwrap();

    assert_eq!(
        exports.import(OWNER, id).unwrap_err(),
        GpuError::Invalid("synchronization self-import")
    );
    assert_eq!(value(&exports.import(IMPORTER, id).unwrap()), 7);
    assert_eq!(
        exports.import(IMPORTER, id).unwrap_err(),
        GpuError::Invalid("duplicate synchronization import")
    );
    assert_eq!(value(&exports.import(OTHER, id).unwrap()), 7);
}

#[test]
fn owner_destroy_under_import_retains_until_the_last_import_drops() {
    let exports = SyncExports::new();
    let concrete = Arc::new(41u64);
    let weak = Arc::downgrade(&concrete);
    let id = exports.export(OWNER, concrete.clone()).unwrap();
    drop(concrete);
    let imported = exports.import(IMPORTER, id).unwrap();

    exports.owner_release(OWNER, id).unwrap();
    assert!(exports.is_live(id));
    assert_eq!(
        exports.import(OTHER, id).unwrap_err(),
        GpuError::Invalid("stale synchronization export")
    );
    assert_eq!(value(&imported), 41);
    drop(imported);
    exports.release_import(IMPORTER, id).unwrap();
    assert!(!exports.is_live(id));
    assert!(weak.upgrade().is_none());
}

#[test]
fn owner_departure_retains_for_importers_and_importer_departure_collects() {
    let exports = SyncExports::new();
    let id = exports.export(OWNER, object(9)).unwrap();
    let imported = exports.import(IMPORTER, id).unwrap();

    exports.forget_session(OWNER);
    assert!(exports.is_live(id));
    assert_eq!(value(&imported), 9);
    drop(imported);
    exports.forget_session(IMPORTER);
    assert!(!exports.is_live(id));
}

#[test]
fn foreign_release_and_stale_operations_are_refused_with_live_controls() {
    let exports = SyncExports::new();
    let id = exports.export(OWNER, object(5)).unwrap();

    assert!(matches!(
        exports.owner_release(OTHER, id),
        Err(GpuError::Invalid(_))
    ));
    assert_eq!(value(&exports.import(IMPORTER, id).unwrap()), 5);
    assert!(matches!(
        exports.release_import(OTHER, id),
        Err(GpuError::Invalid(_))
    ));
    exports.release_import(IMPORTER, id).unwrap();
    exports.owner_release(OWNER, id).unwrap();
    assert!(matches!(
        exports.import(OTHER, id),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        exports.owner_release(OWNER, SyncExportId::from_parts(u64::MAX, 0)),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn clones_share_one_process_global_table() {
    let owner_view = SyncExports::new();
    let importer_view = owner_view.clone();
    let id = owner_view.export(OWNER, object(13)).unwrap();
    assert_eq!(value(&importer_view.import(IMPORTER, id).unwrap()), 13);
}

#[test]
fn timeline_waits_before_and_after_signal() {
    let timeline = Arc::new(TimelineSync::new(2));
    assert_eq!(timeline.wait(2, Duration::ZERO), TimelineWait::Reached);

    let start = Arc::new(Barrier::new(2));
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = {
        let timeline = timeline.clone();
        let start = start.clone();
        thread::spawn(move || {
            start.wait();
            done_tx
                .send(timeline.wait(5, Duration::from_secs(1)))
                .unwrap();
        })
    };
    start.wait();
    timeline.signal(5).unwrap();
    assert_eq!(done_rx.recv().unwrap(), TimelineWait::Reached);
    waiter.join().unwrap();
    assert_eq!(timeline.wait(4, Duration::ZERO), TimelineWait::Reached);
}

#[test]
fn timeline_timeout_and_non_monotonic_signals_are_deterministic() {
    let timeline = TimelineSync::new(7);
    assert_eq!(timeline.wait(8, Duration::ZERO), TimelineWait::Timeout);
    assert!(matches!(timeline.signal(7), Err(GpuError::Invalid(_))));
    assert!(matches!(timeline.signal(6), Err(GpuError::Invalid(_))));
    assert_eq!(timeline.value(), 7);
    timeline.signal(8).unwrap();
    assert_eq!(timeline.wait(8, Duration::ZERO), TimelineWait::Reached);
}

#[test]
fn timeline_survives_owner_destroy_under_import() {
    let exports = SyncExports::new();
    let owner = Arc::new(TimelineSync::new(1));
    let id = exports.export(OWNER, owner.clone()).unwrap();
    let imported = exports
        .import(IMPORTER, id)
        .unwrap()
        .downcast::<TimelineSync>()
        .unwrap();
    exports.owner_release(OWNER, id).unwrap();
    drop(owner);

    imported.signal(3).unwrap();
    assert_eq!(imported.wait(3, Duration::ZERO), TimelineWait::Reached);
    exports.release_import(IMPORTER, id).unwrap();
    assert!(!exports.is_live(id));
}
